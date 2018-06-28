//! Dealing with individual IRC virtual users.

use irc::client::PackedIrcClient;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use comm::{ModemCommand, ContactManagerCommand, ContactFactoryCommand, InitParameters};
use huawei_modem::pdu::{PduAddress, DeliverPdu};
use store::Store;
use failure::Error;
use futures::{self, Future, Async, Poll, Stream};
use futures::future::Either;
use std::default::Default;
use irc::client::{IrcClient, ClientStream, Client};
use irc::client::ext::ClientExt;
use irc::proto::command::Command;
use irc::proto::response::Response;
use irc::proto::message::Message;
use models::Message as OurMessage;
use models::Recipient;
use irc::client::data::config::Config as IrcConfig;
use util::{self, Result};

/// The maximum message size sent over IRC.
static MESSAGE_MAX_LEN: usize = 350;
pub struct ContactManager {
    irc: PackedIrcClient,
    irc_stream: ClientStream,
    admin: String,
    nick: String,
    addr: PduAddress,
    store: Store,
    id: bool,
    admin_is_online: bool,
    connected: bool,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    modem_tx: UnboundedSender<ModemCommand>,
    pub tx: UnboundedSender<ContactManagerCommand>,
    rx: UnboundedReceiver<ContactManagerCommand>
}
impl Future for ContactManager {
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
        while let Async::Ready(cmc) = self.rx.poll().unwrap() {
            let cmc = cmc.ok_or(format_err!("contactmanager rx died"))?;
            self.handle_int_rx(cmc)?;
        }
        while let Async::Ready(_) = self.irc.1.poll()? {}
        Ok(Async::NotReady)
    }
}
impl ContactManager {
    pub fn add_command(&self, cmd: ContactManagerCommand) {
        self.tx.unbounded_send(cmd)
            .unwrap()
    }
    fn send_raw_message(&mut self, msg: &str) -> Result<()> {
        // We need to split messages that are too long to send on IRC up
        // into fragments, as well as splitting them at newlines.
        //
        // Shoutout to sebk from #rust on moznet for providing
        // this nifty implementation!
        for line in msg.lines() {
            let mut last = 0;
            let mut iter = line.char_indices().filter_map(|(i, _)| {
                if i >= last + MESSAGE_MAX_LEN {
                    let part = &line[last..i];
                    last = i;
                    Some(part)
                }
                else if last + MESSAGE_MAX_LEN >= line.len() {
                    let part = &line[last..];
                    last = line.len();
                    Some(part)
                }
                else {
                    None
                }
            });
            for chunk in iter {
                self.irc.0.send_privmsg(&self.admin, chunk)?;
            }
        }
        Ok(())
    }
    fn process_msg_pdu(&mut self, msg: OurMessage, pdu: DeliverPdu) -> Result<()> {
        use huawei_modem::convert::TryFrom;

        // sanity check
        if pdu.originating_address != self.addr {
            return Err(format_err!("PDU for {} sent to ContactManager for {}", pdu.originating_address, self.addr));
        }
        match pdu.get_message_data().decode_message() {
            Ok(m) => {
                if let Some(cd) = m.udh.and_then(|x| x.get_concatenated_sms_data()) {
                    debug!("Message is concatenated: {:?}", cd);
                    let msgs = self.store.get_all_concatenated(&msg.phone_number, cd.reference as _)?;
                    if msgs.len() != (cd.parts as usize) {
                        debug!("Not enough messages: have {}, need {}", msgs.len(), cd.parts);
                        return Ok(());
                    }
                    let mut concatenated = String::new();
                    let mut pdus = vec![];
                    for msg in msgs.iter() {
                        let dec = DeliverPdu::try_from(&msg.pdu)?
                            .get_message_data()
                            .decode_message()?;
                        pdus.push(dec);
                    }
                    pdus.sort_by_key(|p| p.udh.as_ref().unwrap().get_concatenated_sms_data().unwrap().sequence);
                    for pdu in pdus {
                        concatenated.push_str(&pdu.text);
                    }
                    self.send_raw_message(&concatenated)?;
                    for msg in msgs.iter() {
                        self.store.delete_message(msg.id)?;
                    }
                }
                else {
                    self.send_raw_message(&m.text)?;
                    self.store.delete_message(msg.id)?;
                }
            },
            Err(e) => {
                warn!("Error decoding message from {}: {:?}", self.addr, e);
                self.irc.0.send_notice(&self.admin, &format!("Indecipherable message: {}", e))?;
            }
        }
        Ok(())
    }
    fn process_messages(&mut self) -> Result<()> {
        use huawei_modem::convert::TryFrom;

        if !self.connected {
            debug!("Not processing messages yet; not connected");
            return Ok(());
        }
        if !self.admin_is_online {
            debug!("Not processing messages; admin offline");
            return Ok(());
        }

        let msgs = self.store.get_messages_for_recipient(&self.addr)?;
        for msg in msgs {
            debug!("Processing message #{}", msg.id);
            let pdu = DeliverPdu::try_from(&msg.pdu)?;
            self.process_msg_pdu(msg, pdu)?;
        }
        Ok(())
    }
    fn handle_int_rx(&mut self, cmc: ContactManagerCommand) -> Result<()> {
        use self::ContactManagerCommand::*;
        match cmc {
            ProcessMessages => self.process_messages()?,
        }
        Ok(())
    }
    fn change_nick(&mut self, nick: String) -> Result<()> {
        info!("Contact {} changing nick to {}", self.nick, nick);
        self.store.update_recipient_nick(&self.addr, &nick)?;
        self.irc.0.send(Command::NICK(nick))?;
        Ok(())
    }
    fn process_admin_command(&mut self, mesg: String) -> Result<()> {
        if mesg.len() < 1 || mesg.chars().nth(0) != Some('!') {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        match msg[0] {
            "!nick" => {
                if msg.get(1).is_none() {
                    self.irc.0.send_notice(&self.admin, "!nick takes an argument.")?;
                    return Ok(());
                }
                self.change_nick(msg[1].into())?;
                self.irc.0.send_notice(&self.admin, "Done.")?;
            },
            "!die" => {
                self.cf_tx.unbounded_send(ContactFactoryCommand::DropContact(self.addr.clone()))
                    .unwrap();
            },
            unrec => {
                self.irc.0.send_notice(&self.admin, &format!("Unknown command: {}", unrec))?;
            }
        }
        Ok(())
    }
    fn initialize_watch(&mut self) -> Result<()> {
        debug!("Attempting to WATCH +{}", self.admin);
        self.irc.0.send(Command::Raw("WATCH".into(), vec![format!("+{}", self.admin)], None))?;
        Ok(())
    }
    fn handle_irc_message(&mut self, im: Message) -> Result<()> {
        match im.command {
            Command::Response(Response::RPL_ENDOFMOTD, _, _) |
                Command::Response(Response::ERR_NOMOTD, _, _) => {
                debug!("Contact {} connected", self.addr);
                self.connected = true;
                self.process_messages()?;
                self.initialize_watch()?;
            },
            Command::NICK(nick) => {
                if let Some(from) = im.prefix {
                    let from = from.split("!").collect::<Vec<_>>();
                    if let Some(&from) = from.get(0) {
                        if from == self.nick {
                            self.nick = nick;
                        }
                    }
                }
            },
            Command::NOTICE(target, mesg) => {
                if let Some(from) = im.prefix {
                    let from = from.split("!").collect::<Vec<_>>();
                    trace!("{} got NOTICE from {:?} to {}: {}", self.addr, from, target, mesg);
                    if from.len() < 1 {
                        return Ok(());
                    }
                    if from[0] != self.admin {
                        self.irc.0.send_notice(from[0], "You aren't the SMS bridge aministrator, so sending me NOTICEs will do nothing!")?;
                        return Ok(());
                    }
                    if target == self.nick {
                        debug!("Received contact command: {}", mesg);
                        self.process_admin_command(mesg)?;
                    }
                }
            },
            Command::PRIVMSG(target, mesg) => {
                if let Some(from) = im.prefix {
                    let from = from.split("!").collect::<Vec<_>>();
                    trace!("{} got PRIVMSG from {:?} to {}: {}", self.addr, from, target, mesg);
                    if from.len() < 1 {
                        return Ok(());
                    }
                    if from[0] != self.admin {
                        self.irc.0.send_notice(from[0], "Message not delivered; you aren't the SMS bridge administrator!")?;
                        return Ok(());
                    }
                    if target == self.nick {
                        debug!("{} -> {}: {}", from[0], self.addr, mesg); 
                        self.modem_tx.unbounded_send(ModemCommand::SendMessage(self.addr.clone(), mesg)).unwrap();
                    }
                }
            },
            Command::Raw(cmd, args, suffix) => {
                trace!("Raw response: {} {:?} {:?}", cmd, args, suffix);
                if args.len() < 2 {
                    return Ok(());
                }
                match &cmd as &str {
                    "600" | "604" => { // RPL_LOGON / RPL_NOWON
                        if args[1] == self.admin {
                            debug!("Admin {} is online.", self.admin);
                            if !self.admin_is_online {
                                info!("Admin {} has returned; sending queued messages.", self.admin);
                                self.admin_is_online = true;
                                self.process_messages()?;
                            }
                        }
                    },
                    "601" | "605" => { // RPL_LOGOFF / RPL_NOWOFF
                        if args[1] == self.admin {
                            debug!("Admin {} is offline.", self.admin);
                            if self.admin_is_online {
                                self.admin_is_online = false;
                                warn!("Admin {} has gone offline; queuing messages until their return.", self.admin);
                            }
                        }
                    },
                    _ => {}
                }
            },
            Command::Response(Response::ERR_UNKNOWNCOMMAND, args, suffix) => {
                trace!("Unknown command response: {:?} {:?}", args, suffix);
                if args.len() == 2 && args[1] == "WATCH" {
                    warn!("WATCH not supported by server!");
                }
            },
            Command::ERROR(msg) => {
                return Err(format_err!("Error from server: {}", msg));
            },
            _ => {}
        }
        Ok(())
    }
    pub fn new(recip: Recipient, p: InitParameters) -> impl Future<Item = Self, Error = Error> {
        let store = p.store;
        let addr = match util::un_normalize_address(&recip.phone_number)
            .ok_or(format_err!("invalid num {} in db", recip.phone_number)) {
            Ok(r) => r,
            Err(e) => return Either::B(futures::future::err(e.into()))
        };
        let (tx, rx) = mpsc::unbounded();
        let modem_tx = p.cm.modem_tx.clone();
        let cf_tx = p.cm.cf_tx.clone();
        let admin = p.cfg.admin_nick.clone();
        let cfg = Box::into_raw(Box::new(IrcConfig {
            nickname: Some(recip.nick),
            alt_nicks: Some(vec!["smsirc_fallback".to_string()]),
            realname: Some(addr.to_string()),
            server: Some(p.cfg.irc_hostname.clone()),
            password: p.cfg.irc_password.clone(),
            port: p.cfg.irc_port,
            channels: Some(vec![p.cfg.irc_channel.clone()]),
            ..Default::default()
        }));
        // DODGY UNSAFE STUFF: The way IrcClient::new_future works is stupid.
        // However, when the future it returns has resolved, it no longer
        // holds a reference to the IrcConfig. Therefore, we fudge a 'static
        // reference here (to satisfy the stupid method), and deallocate
        // it later, when the future has resolved.
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
                        let nick = cli.0.current_nickname().into();
                        tx.unbounded_send(ContactManagerCommand::ProcessMessages)
                            .unwrap();
                        Ok(ContactManager {
                            irc: cli,
                            irc_stream,
                            id: false,
                            connected: false,
                            // Assume admin is online to start with
                            admin_is_online: true,
                            addr, store, modem_tx, tx, rx, admin, nick, cf_tx
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
