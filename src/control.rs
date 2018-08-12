//! IRC bot that allows users to control the bridge.
//!
//! FIXME: A lot of this module is a copypasta from src/contact.rs and I don't like it :(

use irc::client::PackedIrcClient;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use comm::{ControlBotCommand, ModemCommand, ContactFactoryCommand, InitParameters, WhatsappCommand};
use failure::Error;
use futures::{self, Future, Async, Poll, Stream};
use futures::future::Either;
use irc::client::{IrcClient, ClientStream, Client};
use irc::proto::command::Command;
use irc::proto::response::Response;
use irc::client::data::config::Config as IrcConfig;
use irc::proto::message::Message;
use irc::client::ext::ClientExt;
use util::Result;
use store::Store;

static HELPTEXT: &str = r#"sms-irc help:
[in this admin room]
- !csq: check modem signal quality
- !reg: check modem registration status
- !sms <num>: start a conversation with a given phone number
[in a /NOTICE to one of the ghosts]
- !nick <nick>: change nickname
- !wasetup: set up WhatsApp Web integration
- !walogon: logon to WhatsApp Web using stored credentials
- !wabridge <jid> <#channel>: bridge the WA group <jid> to an IRC channel <#channel>
- !walist: list available WA groups
- !wadel <#channel>: unbridge IRC channel <#channel>
"#;
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
    fn process_admin_command(&mut self, mesg: String) -> Result<()> {
        if mesg.len() < 1 || mesg.chars().nth(0) != Some('!') {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        match msg[0] {
            "!wasetup" => {
                self.wa_tx.unbounded_send(WhatsappCommand::StartRegistration)
                    .unwrap();
            },
            "!walist" => {
                self.wa_tx.unbounded_send(WhatsappCommand::GroupList)
                    .unwrap();
            },
            "!walogon" => {
                self.wa_tx.unbounded_send(WhatsappCommand::LogonIfSaved)
                    .unwrap();
            },
            "!wadel" => {
                if msg.get(1).is_none() {
                    self.irc.0.send_privmsg(&self.chan, "!wadel takes an argument.")?;
                    return Ok(());
                }
                self.wa_tx.unbounded_send(WhatsappCommand::GroupRemove(msg[1].into()))
                    .unwrap();
            },
            "!wabridge" => {
                if msg.get(1).is_none() || msg.get(2).is_none() {
                    self.irc.0.send_privmsg(&self.chan, "!wabridge takes two arguments.")?;
                    return Ok(());
                }
                let jid = match msg[1].parse() {
                    Ok(j) => j,
                    Err(e) => {
                        self.irc.0.send_privmsg(&self.chan, &format!("failed to parse jid: {}", e))?;
                        return Ok(());
                    }
                };
                self.wa_tx.unbounded_send(WhatsappCommand::GroupAssociate(jid, msg[2].into()))
                    .unwrap();
            },
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
                self.irc.0.send_notice(&self.admin, &format!("\x02\x0304{}\x0f", err))?;
            },
            CommandResponse(resp) => {
                self.irc.0.send_privmsg(&self.chan, &format!("{}: {}", self.admin, resp))?;
            },
            CsqResult(sq) => {
                self.irc.0.send_privmsg(&self.chan, &format!("RSSI: \x02{}\x0f | BER: \x02{}\x0f", sq.rssi, sq.ber))?;
            },
            RegResult(st) => {
                self.irc.0.send_privmsg(&self.chan, &format!("Registration state: \x02{}\x0f", st))?;
            },
            ProcessGroups => self.process_groups()?
        }
        Ok(())
    }
    pub fn new(p: InitParameters) -> impl Future<Item = Self, Error = Error> {
        let cf_tx = p.cm.cf_tx.clone();
        let wa_tx = p.cm.wa_tx.clone();
        let m_tx = p.cm.modem_tx.clone();
        let rx = p.cm.cb_rx.take().unwrap();
        let admin = p.cfg.admin_nick.clone();
        let chan = p.cfg.irc_channel.clone();
        let store = p.store.clone();
        let webirc_password = p.cfg.webirc_password.clone();
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
