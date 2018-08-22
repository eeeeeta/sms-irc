//! Modem management.

use huawei_modem::{HuaweiModem, cmd};
use futures::{self, Future, Stream, Poll, Async};
use futures::future::Either;
use tokio_core::reactor::Handle;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use huawei_modem::at::AtResponse;
use comm::{ModemCommand, ContactFactoryCommand, ControlBotCommand, InitParameters};
use tokio_timer::Interval;
use std::time::{Instant, Duration};
use store::Store;
use huawei_modem::cmd::sms::SmsMessage;
use huawei_modem::pdu::{Pdu, PduAddress};
use huawei_modem::gsm_encoding::GsmMessageData;
use huawei_modem::errors::HuaweiError;
use failure::Error;
use util::Result;

pub struct ModemManager {
    modem: HuaweiModem,
    store: Store,
    handle: Handle,
    rx: UnboundedReceiver<ModemCommand>,
    urc_rx: UnboundedReceiver<AtResponse>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    int_tx: UnboundedSender<ModemCommand>,
    cb_tx: UnboundedSender<ControlBotCommand>,
}
impl Future for ModemManager {
    type Item = ();
    type Error = Error;
    
    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(urc) = self.urc_rx.poll().unwrap() {
            let urc = urc.expect("urc_rx stopped producing");
            trace!("received URC: {:?}", urc);
            if let AtResponse::InformationResponse { param, .. } = urc {
                if param == "+CMTI" {
                    debug!("received CMTI indication");
                    self.cmgl();
                }
            }
        }
        while let Async::Ready(msg) = self.rx.poll().unwrap() {
            use self::ModemCommand::*;

            let msg = msg.expect("rx stopped producing");
            match msg {
                DoCmgl => self.cmgl(),
                CmglComplete(msgs) => self.cmgl_complete(msgs)?,
                CmglFailed(e) => self.cmgl_failed(e),
                SendMessage(addr, msg) => self.send_message(addr, msg),
                RequestCsq => self.request_csq(),
                RequestReg => self.request_reg()
            }
        }
        Ok(Async::NotReady)
    }
}
impl ModemManager {
    pub fn new<T>(p: InitParameters<T>) -> impl Future<Item = Self, Error = Error> {
        let mut modem = match HuaweiModem::new_from_path(&p.cfg.modem_path, p.hdl) {
            Ok(m) => m,
            Err(e) => return Either::B(futures::future::err(e.into()))
        };

        let handle = p.hdl.clone();
        let urc_rx = modem.take_urc_rx().unwrap();
        let cs = p.cfg.cmgl_secs;
        let rx = p.cm.modem_rx.take().unwrap();
        let int_tx = p.cm.modem_tx.clone();
        let cf_tx = p.cm.cf_tx.clone();
        let cb_tx = p.cm.cb_tx.clone();

        int_tx.unbounded_send(ModemCommand::DoCmgl).unwrap();
        if let Some(cs) = cs {
            let int_tx_timer = p.cm.modem_tx.clone();
            let timer = Interval::new(Instant::now(), Duration::new(cs as _, 0))
                .map_err(|e| {
                    error!("CMGL timer failed: {}", e);
                    panic!("timer failed!");
                }).for_each(move |_| {
                    trace!("CMGL timer triggered.");
                    int_tx_timer.unbounded_send(ModemCommand::DoCmgl).unwrap();
                    Ok(())
                });
            p.hdl.spawn(timer);
        }
        let store = p.store;
        let fut = cmd::sms::set_sms_textmode(&mut modem, false)
            .map_err(|e| Error::from(e))
            .join(cmd::sms::set_new_message_indications(&mut modem,
                                                        cmd::sms::NewMessageNotification::SendDirectlyOrBuffer,
                                                        cmd::sms::NewMessageStorage::StoreAndNotify)
                  .map_err(|e| Error::from(e)))
            .then(move |res| {
                if let Err(e) = res {
                    warn!("Failed to set +CNMI: {}", e);
                    if cs.is_none() {
                        error!("+CNMI support is not available, and cmgl_secs is not provided in the config");
                        return Err(format_err!("no CNMI support, and no cmgl_secs"));
                    }
                }
                Ok(Self {
                    modem, rx, store, cf_tx, urc_rx, handle, int_tx, cb_tx
                })
            });
        Either::A(fut)
    }
    fn request_reg(&mut self) {
        let tx = self.cb_tx.clone();
        let fut = cmd::network::get_registration(&mut self.modem)
            .then(move |res| {
                match res {
                    Ok(res) => {
                        let res = format!("Registration state: \x02{}\x0f", res);
                        tx.unbounded_send(ControlBotCommand::CommandResponse(res)).unwrap();
                    },
                    Err(e) => warn!("Error getting registration: {}", e)
                }
                Ok(())
            });
        self.handle.spawn(fut);
    }
    fn request_csq(&mut self) {
        let tx = self.cb_tx.clone();
        let fut = cmd::network::get_signal_quality(&mut self.modem)
            .then(move |res| {
                match res {
                    Ok(res) => {
                        let res = format!("RSSI: \x02{}\x0f | BER: \x02{}\x0f", res.rssi, res.ber);
                        tx.unbounded_send(ControlBotCommand::CommandResponse(res)).unwrap();
                    }
                    Err(e) => warn!("Error getting signal quality: {}", e)
                }
                Ok(())
            });
        self.handle.spawn(fut);
    }
    fn cmgl_complete(&mut self, msgs: Vec<SmsMessage>) -> Result<()> {
        use huawei_modem::cmd::sms::{MessageStatus, DeletionOptions};

        debug!("+CMGL complete");
        trace!("messages received from CMGL: {:?}", msgs);
        for msg in msgs {
            trace!("processing message: {:?}", msg);
            if msg.status != MessageStatus::ReceivedUnread {
                continue;
            }
            let data = msg.pdu.get_message_data();
            let csms_data = match data.decode_message() {
                Ok(m) => {
                    m.udh
                        .and_then(|x| x.get_concatenated_sms_data())
                        .map(|x| x.reference as i32)
                },
                Err(e) => {
                    // The error is reported when the message is sent
                    // via the ContactManager, not here.
                    debug!("Error decoding message - but it'll be reported later: {:?}", e);
                    None
                }
            };
            if let Some(d) = csms_data {
                trace!("Message is concatenated: {:?}", d);
            }
            let addr = msg.pdu.originating_address;
            self.store.store_message(&addr, &msg.raw_pdu, csms_data)?;
        }
        self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages).unwrap();
        let fut = cmd::sms::del_sms_pdu(&mut self.modem, DeletionOptions::DeleteReadAndOutgoing)
            .map_err(|e| {
                warn!("Failed to delete messages: {}", e);
            });
        self.handle.spawn(fut);
        Ok(())
    }
    fn cmgl_failed(&mut self, e: HuaweiError) {
        // FIXME: retry perhaps?
        error!("+CMGL failed: {}", e);
    }
    fn send_message(&mut self, addr: PduAddress, msg: String) {
        let data = GsmMessageData::encode_message(&msg);
        let parts = data.len();
        debug!("Sending {}-part message to {}...", parts, addr);
        trace!("Message content: {}", msg);
        let mut futs = vec![];
        for (i, part) in data.into_iter().enumerate() {
            debug!("Sending part {}/{} of message to {}...", i+1, parts, addr);
            let pdu = Pdu::make_simple_message(addr.clone(), part);
            trace!("PDU: {:?}", pdu);
            futs.push(cmd::sms::send_sms_pdu(&mut self.modem, &pdu)
                      .map_err(|e| e.into()));
        }
        let a1 = addr.clone();
        let cb_tx = self.cb_tx.clone();
        let fut = futures::future::join_all(futs)
            .map(move |res| {
                info!("Message to {} sent!", a1);
                debug!("Message ids: {:?}", res);
            }).map_err(move |e: ::failure::Error| {
                // FIXME: retrying?
                warn!("Failed to send message to {}: {}", addr, e);
                let emsg = format!("Failed to send message to {}: {}", addr, e);
                cb_tx.unbounded_send(ControlBotCommand::ReportFailure(emsg))
                    .unwrap();
            });
        self.handle.spawn(fut);
    }
    fn cmgl(&mut self) {
        use huawei_modem::cmd::sms::MessageStatus;

        let tx = self.int_tx.clone();
        let fut = cmd::sms::list_sms_pdu(&mut self.modem, MessageStatus::All)
            .then(move |results| {
                match results {
                    Ok(results) => {
                        tx.unbounded_send(
                            ModemCommand::CmglComplete(results)).unwrap();
                    },
                    Err(e) => {
                        tx.unbounded_send(
                            ModemCommand::CmglFailed(e)).unwrap();
                    }
                }
                let res: ::std::result::Result<(), ()> = Ok(());
                res
            });
        self.handle.spawn(fut);
    }
}
