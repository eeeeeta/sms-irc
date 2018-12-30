//! Modem management.

use huawei_modem::{HuaweiModem, cmd};
use futures::{self, Future, Stream, Poll, Async, IntoFuture};
use tokio_core::reactor::Handle;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use huawei_modem::at::AtResponse;
use comm::{ModemCommand, ContactFactoryCommand, ControlBotCommand, InitParameters};
use tokio_timer::{Delay, Interval};
use std::time::{Instant, Duration};
use store::Store;
use huawei_modem::cmd::sms::SmsMessage;
use huawei_modem::pdu::{Pdu, PduAddress};
use huawei_modem::gsm_encoding::GsmMessageData;
use huawei_modem::errors::HuaweiError;
use failure::Error;
use util::Result;
use std::mem;

enum ModemInner {
    Uninitialized,
    Disabled,
    Waiting(Delay),
    Initializing(Box<Future<Item = HuaweiModem, Error = Error>>),
    Running {
        modem: HuaweiModem,
        urc_rx: UnboundedReceiver<AtResponse>
    },
}
impl ModemInner {
    fn init_future(path: &str, hdl: &Handle, has_cs: bool) -> Box<Future<Item = HuaweiModem, Error = Error>> {
        info!("Initializing modem {}", path);
        let modem = HuaweiModem::new_from_path(path, hdl);
        let fut = modem.into_future()
            .map_err(|e| Error::from(e))
            .and_then(move |mut modem| {
                cmd::sms::set_sms_textmode(&mut modem, false)
                    .map_err(|e| Error::from(e))
                    .join(cmd::sms::set_new_message_indications(&mut modem,
                                                                cmd::sms::NewMessageNotification::SendDirectlyOrBuffer,
                                                                cmd::sms::NewMessageStorage::StoreAndNotify)
                          .map_err(|e| Error::from(e)))
                    .then(move |res| {
                        if let Err(e) = res {
                            warn!("Failed to set +CNMI: {}", e);
                            if !has_cs {
                                error!("+CNMI support is not available, and cmgl_secs is not provided in the config");
                                return Err(format_err!("no CNMI support, and no cmgl_secs"));
                            }
                        }
                        Ok(modem)
                    })
            });
        Box::new(fut)
    }
    fn make_delay(delay_ms: u32) -> Delay {
        Delay::new(Instant::now() + Duration::from_millis(delay_ms as _))
    }
    pub fn report_error(&mut self, e: Error, delay_ms: u32) {
        error!("Modem error: {}", e);
        *self = ModemInner::Waiting(Self::make_delay(delay_ms));
    }
    pub fn get_urc_rx(&mut self) -> Option<&mut UnboundedReceiver<AtResponse>> {
        if let ModemInner::Running { ref mut urc_rx, .. } = *self {
            Some(urc_rx)
        }
        else {
            None
        }
    }
    pub fn get_modem(&mut self) -> Result<&mut HuaweiModem> {
        if let ModemInner::Running { ref mut modem, .. } = *self {
            Ok(modem)
        }
        else {
            Err(format_err!("Modem is not initialized"))
        }
    }
    // return value: whether or not the modem was just freshly reinitialized
    pub fn poll(&mut self, modem_path: &Option<String>, hdl: &Handle, has_cs: bool, delay_ms: u32) -> bool {
        use self::ModemInner::*;

        loop {
            match mem::replace(self, Uninitialized) {
                Uninitialized => {
                    if let Some(ref path) = modem_path {
                        *self = Initializing(Self::init_future(path, hdl, has_cs));
                    }
                    else {
                        info!("Modem is disabled");
                        *self = Disabled;
                        break;
                    }
                },
                Waiting(mut delay) => {
                    match delay.poll() {
                        Ok(Async::Ready(_)) => {
                            *self = Uninitialized;
                        },
                        Ok(Async::NotReady) => {
                            *self = Waiting(delay);
                            break;
                        },
                        Err(e) => {
                            error!("Modem delay timer failed: {}", e);
                            *self = Waiting(Self::make_delay(delay_ms));
                        }
                    }
                },
                Initializing(mut fut) => {
                    match fut.poll() {
                        Ok(Async::Ready(mut modem)) => {
                            let urc_rx = modem.take_urc_rx().unwrap();
                            *self = Running {
                                modem, urc_rx
                            };
                            return true;
                        },
                        Ok(Async::NotReady) => {
                            *self = Initializing(fut);
                            break;
                        },
                        Err(e) => {
                            error!("Modem initialization failed: {}", e);
                            *self = Waiting(Self::make_delay(delay_ms));
                        }
                    }
                },
                x => {
                    *self = x;
                    break;
                }
            }
        }
        false
    }
}
macro_rules! get_modem {
    ($self:ident, $desc:expr) => {
        match $self.inner.get_modem() {
            Ok(m) => m,
            Err(e) => {
                warn!("Modem operation failed: {}", e);
                let err = format!("{} failed: {}", $desc, e);
                $self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err))
                    .unwrap();
                return;
            }
        }
    }
}
pub struct ModemManager {
    inner: ModemInner,
    store: Store,
    handle: Handle,
    modem_path: Option<String>,
    delay_ms: u32,
    has_cs: bool,
    rx: UnboundedReceiver<ModemCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    int_tx: UnboundedSender<ModemCommand>,
    cb_tx: UnboundedSender<ControlBotCommand>,
}
impl Future for ModemManager {
    type Item = ();
    type Error = Error;
    
    fn poll(&mut self) -> Poll<(), Error> {
        self.poll_modem();
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
    fn poll_modem(&mut self) {
        if self.inner.poll(&self.modem_path, &self.handle, self.has_cs, self.delay_ms) {
            self.cmgl();
        }
        if let Err(e) = self.poll_urc_rx() {
            self.report_modem_error(e);
        }
    }
    fn report_modem_error(&mut self, err: Error) {
        self.inner.report_error(err, self.delay_ms);
        self.poll_modem();
    }
    fn poll_urc_rx(&mut self) -> Result<()> {
        let mut do_cmgl = false;
        if let Some(urc_rx) = self.inner.get_urc_rx() {
            while let Async::Ready(urc) = urc_rx.poll().unwrap() {
                let urc = urc.ok_or(format_err!("urc_rx stopped producing"))?;
                trace!("received URC: {:?}", urc);
                if let AtResponse::InformationResponse { param, .. } = urc {
                    if param == "+CMTI" {
                        debug!("received CMTI indication");
                        do_cmgl = true;
                    }
                }
            }
        }
        if do_cmgl {
            self.cmgl();
        }
        Ok(())
    }
    pub fn new<T>(p: InitParameters<T>) -> Self {
        let modem_path = p.cfg.modem_path.clone();
        let handle = p.hdl.clone();
        let cs = p.cfg.cmgl_secs;
        let delay_ms = p.cfg.modem_restart_delay_ms.unwrap_or(5000);
        let has_cs = cs.is_some();
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
        let inner = ModemInner::Uninitialized;
        Self {
            rx, store, cf_tx, handle, int_tx, cb_tx, inner, has_cs, modem_path, delay_ms
        }
    }
    fn request_reg(&mut self) {
        let tx = self.cb_tx.clone();
        let mut modem = get_modem!(self, "Getting registration");
        let fut = cmd::network::get_registration(&mut modem)
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
        let mut modem = get_modem!(self, "Getting signal quality");
        let fut = cmd::network::get_signal_quality(&mut modem)
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
        let mut modem = match self.inner.get_modem() {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to delete messages: {}", e);
                return Ok(());
            }
        };
        let fut = cmd::sms::del_sms_pdu(&mut modem, DeletionOptions::DeleteReadAndOutgoing)
            .map_err(|e| {
                warn!("Failed to delete messages: {}", e);
            });
        self.handle.spawn(fut);
        Ok(())
    }
    fn cmgl_failed(&mut self, e: HuaweiError) {
        self.report_modem_error(format_err!("+CMGL failed: {}", e));
    }
    fn send_message(&mut self, addr: PduAddress, msg: String) {
        let data = GsmMessageData::encode_message(&msg);
        let parts = data.len();
        let mut modem = get_modem!(self, "Sending message");
        debug!("Sending {}-part message to {}...", parts, addr);
        trace!("Message content: {}", msg);
        let mut futs = vec![];
        for (i, part) in data.into_iter().enumerate() {
            debug!("Sending part {}/{} of message to {}...", i+1, parts, addr);
            let pdu = Pdu::make_simple_message(addr.clone(), part);
            trace!("PDU: {:?}", pdu);
            futs.push(cmd::sms::send_sms_pdu(&mut modem, &pdu)
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

        if let Ok(mut modem) = self.inner.get_modem() {
            let tx = self.int_tx.clone();
            let fut = cmd::sms::list_sms_pdu(&mut modem, MessageStatus::All)
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
        else {
            debug!("+CMGL failed due to uninitialized modem");
        }
    }
}
