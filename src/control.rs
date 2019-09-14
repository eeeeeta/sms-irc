//! IRC bot that allows users to control the bridge.
//!
//! FIXME: A lot of this module is a copypasta from src/contact.rs and I don't like it :(

use irc::client::PackedIrcClient;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use crate::comm::{ControlBotCommand, ModemCommand, ContactFactoryCommand, InitParameters, WhatsappCommand};
use failure::Error;
use futures::{self, Future, Async, Poll, Stream};
use futures::future::Either;
use irc::client::{IrcClient, ClientStream, Client};
use irc::proto::command::Command;
use irc::proto::response::Response;
use irc::client::data::config::Config as IrcConfig;
use irc::proto::message::Message;
use irc::client::ext::ClientExt;
use crate::util::Result;
use crate::config::IrcClientConfig;
use crate::store::Store;
use crate::control_common::ControlCommon;

pub struct ControlBot {
    irc: PackedIrcClient,
    irc_stream: ClientStream,
    chan: String,
    admin: String,
    channels: Vec<String>,
    log_backlog: Vec<String>,
    store: Store,
    id: bool,
    connected: bool,
    webirc_password: Option<String>,
    rx: UnboundedReceiver<ControlBotCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    wa_tx: UnboundedSender<WhatsappCommand>,
    m_tx: UnboundedSender<ModemCommand>
}
impl Future for ControlBot {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        if !self.id {
            if let Some(ref pw) = self.webirc_password {
                self.irc.0.send(Command::Raw("WEBIRC".into(), vec![pw.to_string(), "sms-irc".into(), "sms-irc.theta.eu.org".into(), "127.0.0.1".into()], None))?;
            }
            self.irc.0.identify()?;
            self.id = true;
        }
        while let Async::Ready(_) = self.irc.1.poll()? {}
        while let Async::Ready(res) = self.irc_stream.poll()? {
            let msg = res.ok_or(format_err!("irc_stream stopped"))?;
            self.handle_irc_message(msg)?;
        }
        while let Async::Ready(cbc) = self.rx.poll().unwrap() {
            let cbc = cbc.ok_or(format_err!("controlbot rx died"))?;
            self.handle_int_rx(cbc)?;
        }
        Ok(Async::NotReady)

    }
}
impl ControlCommon for ControlBot {
    fn cf_send(&mut self, c: ContactFactoryCommand) {
        self.cf_tx.unbounded_send(c)
            .unwrap()
    }
    fn wa_send(&mut self, c: WhatsappCommand) { 
        self.wa_tx.unbounded_send(c)
            .unwrap()
    }
    fn m_send(&mut self, c: ModemCommand) {
        self.m_tx.unbounded_send(c)
            .unwrap()
    }
    fn control_response(&mut self, msg: &str) -> Result<()> {
        self.irc.0.send_notice(&self.admin, msg)?;
        Ok(())
    }
}
impl ControlBot {
    // FIXME: this is yet more horrible copypasta :<
    fn process_groups(&mut self) -> Result<()> {
        let mut chans = vec![];
        for grp in self.store.get_all_groups()? {
            self.irc.0.send_join(&grp.channel)?;
            chans.push(grp.channel);
        }
        for ch in ::std::mem::replace(&mut self.channels, chans) {
            if !self.channels.contains(&ch) {
                self.irc.0.send_part(&ch)?;
            }
        }
        Ok(())
    }
    fn handle_irc_message(&mut self, im: Message) -> Result<()> {
        match im.command {
            Command::Response(Response::RPL_ENDOFMOTD, _, _) |
                Command::Response(Response::ERR_NOMOTD, _, _) => {
                debug!("Control bot connected");
                self.connected = true;
                self.clear_log_backlog()?;
                self.process_groups()?;
            },
            Command::PRIVMSG(target, mesg) => {
                if let Some(from) = im.prefix {
                    let from = from.split("!").collect::<Vec<_>>();
                    trace!("ctl got PRIVMSG from {:?} to {}: {}", from, target, mesg);
                    if from.len() < 1 {
                        return Ok(());
                    }
                    if from[0] != self.admin {
                        return Ok(());
                    }
                    if target == self.chan {
                        debug!("Received control command: {}", mesg);
                        self.process_admin_command(mesg)?;
                    }
                    else if self.channels.contains(&target) {
                        debug!("Received group message in {}: {}", target, mesg);
                        self.wa_tx.unbounded_send(WhatsappCommand::SendGroupMessage(target, mesg))
                            .unwrap();
                    }
                    else {
                        warn!("Received unsolicited message to {}: {}", target, mesg);
                    }
                }
            },
            Command::ERROR(msg) => {
                return Err(format_err!("Error from server: {}", msg));
            },
            _ => {}
        }
        Ok(())
    }
    fn clear_log_backlog(&mut self) -> Result<()> {
        for log in self.log_backlog.drain(0..) {
            self.irc.0.send_notice(&self.chan, &log)?;
        }
        Ok(())
    }
    fn handle_int_rx(&mut self, m: ControlBotCommand) -> Result<()> {
        use self::ControlBotCommand::*;

        match m {
            Log(log) => {
                if !self.connected {
                    self.log_backlog.push(log);
                }
                else {
                    self.irc.0.send_notice(&self.chan, &log)?;
                }
            },
            ReportFailure(err) => {
                self.irc.0.send_privmsg(&self.admin, &format!("{}: \x02\x0304{}\x0f", self.admin, err))?;
            },
            CommandResponse(resp) => {
                self.control_response(&resp)?;
            },
            ProcessGroups => self.process_groups()?
        }
        Ok(())
    }
    pub fn new(p: InitParameters<IrcClientConfig>) -> impl Future<Item = Self, Error = Error> {
        let cf_tx = p.cm.cf_tx.clone();
        let wa_tx = p.cm.wa_tx.clone();
        let m_tx = p.cm.modem_tx.clone();
        let rx = p.cm.cb_rx.take().unwrap();
        let admin = p.cfg2.admin_nick.clone();
        let chan = p.cfg2.irc_channel.clone();
        let store = p.store.clone();
        let webirc_password = p.cfg2.webirc_password.clone();
        let cfg = Box::into_raw(Box::new(IrcConfig {
            nickname: Some(p.cfg2.control_bot_nick.clone().unwrap_or("smsirc".into())),
            realname: Some("smsirc control bot".into()),
            server: Some(p.cfg2.irc_hostname.clone()),
            password: p.cfg2.irc_password.clone(),
            port: p.cfg2.irc_port,
            channels: Some(vec![p.cfg2.irc_channel.clone()]),
            ..Default::default()
        }));
        // DODGY UNSAFE STUFF: see src/contact.rs
        let cfgb: &'static IrcConfig = unsafe { &*cfg };
        let fut = match IrcClient::new_future(p.hdl.clone(), cfgb) {
            Ok(r) => r,
            Err(e) => return Either::B(futures::future::err(e.into()))
        };
        let fut = fut
            .then(move |res| {
                let _ = unsafe { Box::from_raw(cfg) };
                match res {
                    Ok(cli) => {
                        let irc_stream = cli.0.stream();
                        Ok(ControlBot {
                            irc: cli,
                            irc_stream,
                            id: false,
                            connected: false,
                            channels: vec![],
                            log_backlog: vec![],
                            cf_tx, m_tx, rx, admin, chan, store, wa_tx, webirc_password
                        })
                    },
                    Err(e) => {
                        Err(e.into())
                    }
                }
            });
        Either::A(fut)
    }
}
