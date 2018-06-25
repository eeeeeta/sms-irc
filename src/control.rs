//! IRC bot that allows users to control the bridge.
//!
//! A lot of this module is a copypasta from src/contact.rs and I don't like it :(

use irc::client::PackedIrcClient;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use comm::{ControlBotCommand, ModemCommand, ContactFactoryCommand, InitParameters};
use failure::Error;
use futures::{self, Future, Async, Poll, Stream};
use futures::future::Either;
use irc::client::{IrcClient, ClientStream, Client};
use irc::proto::command::Command;
use irc::client::data::config::Config as IrcConfig;
use irc::proto::message::Message;
use irc::client::ext::ClientExt;
use util::Result;

static HELPTEXT: &str = r#"sms-irc help:
[in this admin room]
- !csq: check modem signal quality
- !reg: check modem registration status
- !sms <num>: start a conversation with a given phone number
[in a /NOTICE to one of the ghosts]
- !nick <nick>: change nickname"#;
pub struct ControlBot {
    irc: PackedIrcClient,
    irc_stream: ClientStream,
    chan: String,
    admin: String,
    id: bool,
    rx: UnboundedReceiver<ControlBotCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    m_tx: UnboundedSender<ModemCommand>
}
impl Future for ControlBot {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        if !self.id {
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
impl ControlBot {
    fn process_admin_command(&mut self, mesg: String) -> Result<()> {
        if mesg.len() < 1 || mesg.chars().nth(0) != Some('!') {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        match msg[0] {
            "!csq" => {
                self.m_tx.unbounded_send(ModemCommand::RequestCsq).unwrap();
            },
            "!reg" => {
                self.m_tx.unbounded_send(ModemCommand::RequestReg).unwrap();
            },
            "!sms" => {
                if msg.get(1).is_none() {
                    self.irc.0.send_privmsg(&self.chan, "!sms takes an argument.")?;
                    return Ok(());
                }
                let addr = msg[1].parse().unwrap();
                self.cf_tx.unbounded_send(ContactFactoryCommand::MakeContact(addr)).unwrap();
            },
            "!help" => {
                for line in HELPTEXT.lines() {
                    self.irc.0.send_privmsg(&self.chan, line)?;
                }
            },
            unrec => {
                self.irc.0.send_privmsg(&self.chan, &format!("Unknown command: {}", unrec))?;
            }
        }
        Ok(())
    }
    fn handle_irc_message(&mut self, im: Message) -> Result<()> {
        match im.command {
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
                }
            },
            Command::ERROR(msg) => {
                return Err(format_err!("Error from server: {}", msg));
            },
            _ => {}
        }
        Ok(())
    }
    fn handle_int_rx(&mut self, m: ControlBotCommand) -> Result<()> {
        use self::ControlBotCommand::*;

        match m {
            Log(log) => {
                self.irc.0.send_notice(&self.chan, &log)?;
            },
            CsqResult(sq) => {
                self.irc.0.send_privmsg(&self.chan, &format!("RSSI: {} | BER: {}", sq.rssi, sq.ber))?;
            },
            RegResult(st) => {
                self.irc.0.send_privmsg(&self.chan, &format!("Registration state: {}", st))?;
            },
        }
        Ok(())
    }
    pub fn new(p: InitParameters) -> impl Future<Item = Self, Error = Error> {
        let cf_tx = p.cm.cf_tx.clone();
        let m_tx = p.cm.modem_tx.clone();
        let rx = p.cm.cb_rx.take().unwrap();
        let admin = p.cfg.admin_nick.clone();
        let chan = p.cfg.irc_channel.clone();
        let cfg = Box::into_raw(Box::new(IrcConfig {
            nickname: Some(p.cfg.control_bot_nick.clone().unwrap_or("smsirc".into())),
            realname: Some("smsirc control bot".into()),
            server: Some(p.cfg.irc_hostname.clone()),
            password: p.cfg.irc_password.clone(),
            port: p.cfg.irc_port,
            channels: Some(vec![p.cfg.irc_channel.clone()]),
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
                            cf_tx, m_tx, rx, admin, chan
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
