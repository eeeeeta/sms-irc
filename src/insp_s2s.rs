//! InspIRCd server-to-server linking.
use tokio_io::AsyncRead;
use tokio_core::net::TcpStream;
use tokio_io::codec::Framed;
use irc::proto::IrcCodec;
use irc::proto::message::Message;
use irc::proto::command::Command;
use futures::{Future, Async, Poll, Stream, Sink, AsyncSink, self};
use futures::future::Either;
use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use comm::{ControlBotCommand, ContactFactoryCommand, InitParameters, WhatsappCommand, ModemCommand};
use store::Store;
use huawei_modem::pdu::{PduAddress, DeliverPdu};
use std::collections::HashMap;
use failure::Error;
use models::Recipient;
use util::{self, Result};
use contact_common::ContactManagerManager;
use sender_common::Sender;
use control_common::ControlCommon;
use insp_user::InspUser;
use config::InspConfig;
use std::net::{SocketAddr, ToSocketAddrs};

pub static INSP_PROTOCOL_CAPAB: &str = "PROTOCOL=1203";
struct InspContact {
    uuid: String,
    wa_mode: bool
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LinkState {
    TcpConnected,
    ServerSent,
    BurstSent,
    Linked
}

pub struct InspLink {
    conn: Framed<TcpStream, IrcCodec>,
    cfg: InspConfig,
    control_uuid: String,
    next_user_id: u32,
    cf_rx: UnboundedReceiver<ContactFactoryCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    cb_rx: UnboundedReceiver<ControlBotCommand>,
    wa_tx: UnboundedSender<WhatsappCommand>,
    m_tx: UnboundedSender<ModemCommand>,
    // map of UID -> InspUser
    users: HashMap<String, InspUser>,
    contacts: HashMap<PduAddress, InspContact>,
    contacts_uuid_pdua: HashMap<String, PduAddress>,
    store: Store,
    outbox: Vec<Message>,
    channels: Vec<String>,
    state: LinkState,
}

impl Future for InspLink {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        if self.state == LinkState::TcpConnected {
            self.do_capab_and_server()?;
        }
        while let Async::Ready(msg) = self.conn.poll()? {
            let msg = msg.ok_or(format_err!("disconnected"))?;
            self.handle_remote_message(msg)?;
        }
        if self.state == LinkState::Linked {
            while let Async::Ready(cbc) = self.cb_rx.poll().unwrap() {
                let cbc = cbc.ok_or(format_err!("cb_rx stopped"))?;
                self.handle_cb_command(cbc)?;
            }
            while let Async::Ready(cfc) = self.cf_rx.poll().unwrap() {
                let cfc = cfc.ok_or(format_err!("cf_rx stopped"))?;
                self.handle_cf_command(cfc)?;
            }
        }
        for msg in ::std::mem::replace(&mut self.outbox, vec![]) {
            match self.conn.start_send(msg)? {
                AsyncSink::Ready => {},
                AsyncSink::NotReady(val) => {
                    self.outbox.push(val);
                }
            }
        }
        self.conn.poll_complete()?;
        Ok(Async::NotReady)
    }
}
impl ContactManagerManager for InspLink {
    fn setup_contact_for(&mut self, recip: Recipient, addr: PduAddress) -> Result<()> {
        let user = InspUser::new_from_recipient(addr.clone(), recip.nick);
        let uuid = self.new_user(user)?;
        self.contacts.insert(addr.clone(), InspContact {
            uuid: uuid.clone(),
            wa_mode: false
        });
        self.contacts_uuid_pdua.insert(uuid, addr);
        Ok(())
    }
    fn remove_contact_for(&mut self, addr: &PduAddress) -> Result<()> {
        if let Some(uu) = self.contacts.get(addr).map(|x| x.uuid.clone()) {
            self.outbox.push(Message::new(Some(&uu), "QUIT", vec![], Some("Contact removed"))?);
            self.remove_user(&uu, false)?;
        }
        self.contacts.remove(&addr);
        self.contacts_uuid_pdua.retain(|_, v| v != addr);
        Ok(())
    }
    fn has_contact(&mut self, addr: &PduAddress) -> bool {
        self.contacts.get(addr).is_some()
    }
    fn store(&mut self) -> &mut Store {
        &mut self.store
    }
}
impl ControlCommon for InspLink {
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand> {
        &mut self.wa_tx
    }
    fn cf_tx(&mut self) -> &mut UnboundedSender<ContactFactoryCommand> {
        &mut self.cf_tx
    }
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand> {
        &mut self.m_tx
    }
    fn send_cb_message(&mut self, msg: &str) -> Result<()> {
        self.outbox.push(Message::new(Some(&self.control_uuid), "PRIVMSG", vec![&self.cfg.log_chan], Some(msg))?);
        Ok(())
    }
}
impl Sender for InspLink {
    fn report_error(&mut self, uid: &str, err: String) -> Result<()> {
        let admin = self.cfg.admin_nick.clone();
        self.contact_message(uid, "NOTICE", &admin, &err)?;
        Ok(())
    }
    fn store(&mut self) -> &mut Store {
        &mut self.store
    }
    fn private_target(&mut self) -> String {
        if let Some(admu) = self.admin_uuid() {
            admu
        }
        else {
            warn!("Admin not available; sending messages to {} instead", self.cfg.log_chan);
            self.cfg.log_chan.clone()
        }
    }
    fn send_irc_message(&mut self, uid: &str, to: &str, msg: &str) -> Result<()> {
        self.contact_message(uid, "PRIVMSG", to, msg)?;
        Ok(())
    }
}
impl InspLink {
    fn _make_addr_and_codec(cfg: &InspConfig) -> Result<(SocketAddr, IrcCodec)> {
        let addr = (&cfg.hostname as &str, cfg.port)
            .to_socket_addrs()?
            .nth(0)
            .ok_or(format_err!("no addresses found"))?;
        let codec = IrcCodec::new("utf8")?;
        Ok((addr, codec))
    }
    pub fn new(p: InitParameters<InspConfig>) -> impl Future<Item = Self, Error = Error> {
        let store = p.store;
        let cfg = p.cfg2.clone();
        let cf_rx = p.cm.cf_rx.take().unwrap();
        let cb_rx = p.cm.cb_rx.take().unwrap();
        let cf_tx = p.cm.cf_tx.clone();
        let wa_tx = p.cm.wa_tx.clone();
        let m_tx = p.cm.modem_tx.clone();
        let (addr, codec) = match Self::_make_addr_and_codec(&cfg) {
            Ok(x) => x,
            Err(e) => return Either::B(futures::future::err(e))
        };
        let control_uuid = format!("{}00000", cfg.sid);
        info!("Connecting to {}", addr);
        let fut = TcpStream::connect(&addr, p.hdl)
            .map(|res| {
                Self {
                    conn: res.framed(codec),
                    cfg,
                    control_uuid,
                    next_user_id: 1,
                    cf_rx, cf_tx, cb_rx, wa_tx, m_tx,
                    users: HashMap::new(),
                    contacts: HashMap::new(),
                    contacts_uuid_pdua: HashMap::new(),
                    store,
                    outbox: vec![],
                    channels: vec![],
                    state: LinkState::TcpConnected
                }
            })
            .map_err(|e| e.into());
        Either::A(fut)
    }
    fn process_contact_admin_command(&mut self, addr: PduAddress, mesg: String) -> Result<()> {
        use chrono::Utc;

        // FIXME: this is a bit copypasta-y
        let uid = self.contacts.get(&addr)
            .map(|x| x.uuid.clone())
            .expect("pcac called with invalid uid");
        let auid = match self.admin_uuid() {
            Some(x) => x,
            None => {
                warn!("pcac() called with no admin_uuid?");
                return Ok(());
            }
        };
        if mesg.len() < 1 || mesg.chars().nth(0) != Some('!') {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        match msg[0] {
            "!nick" => {
                if msg.get(1).is_none() {
                    self.contact_message(&uid, "NOTICE", &auid, "!nick takes an argument.")?;
                    return Ok(());
                }
                let u = self.users.get_mut(&uid).unwrap();
                u.nick = msg[1].into();
                let ts = Utc::now().timestamp().to_string();
                self.outbox.push(Message::new(Some(&uid), "NICK", vec![&msg[1], &ts], None)?);
                self.store.update_recipient_nick(&addr, &msg[1])?;
            },
            "!wa" => {
                let state = {
                    let mut ct = self.contacts.get_mut(&addr).unwrap();
                    ct.wa_mode = !ct.wa_mode;
                    if ct.wa_mode { "ENABLED" } else { "DISABLED" }
                };
                self.contact_message(&uid, "NOTICE", &auid, &format!("WhatsApp mode: {}", state))?;
            },
            "!die" => {
                self.drop_contact(addr)?;
            },
            unrec => {
                self.contact_message(&uid, "NOTICE", &auid, &format!("Unknown command: {}", unrec))?;
            },
        }
        Ok(())
    }
    fn remove_user(&mut self, uuid: &str, recreate: bool) -> Result<()> {
        debug!("Removing user {} with recreate {}", uuid, recreate);
        let addr = if let Some(pdua) = self.contacts_uuid_pdua.get(uuid) {
            self.contacts.remove(&pdua);
            Some(pdua.clone())
        }
        else {
            None
        };
        self.contacts_uuid_pdua.remove(uuid);
        self.users.remove(uuid);
        if let Some(a) = addr {
            if recreate {
                info!("Contact for {} removed; recreating", a);
                self.make_contact(a)?;
            }
        }
        Ok(())
    }
    fn handle_remote_message(&mut self, m: Message) -> Result<()> {
        let prefix = if let Some(p) = m.prefix {
            p
        }
        else {
            match m.command {
                Command::ERROR(details) => {
                    Err(format_err!("Error from server: {}", details))?
                },
                Command::Raw(cmd, args, suffix) => {
                    match &cmd as &str {
                        "SERVER" => {
                            if args.len() != 4 {
                                Err(format_err!("Received invalid 1-to-1 SERVER: {:?}", args))?
                            }
                            if args[1] != self.cfg.recvpass {
                                Err(format_err!("Invalid password from remote server"))?
                            }
                            if args[2] != "0" {
                                Err(format_err!("Received 1-to-1 SERVER with hopcount != 0: {:?}", args))?
                            }
                            info!("Received SERVER");
                            self.do_burst()?;
                            self.state = LinkState::BurstSent;
                        },
                        "CAPAB" => {
                            debug!("Received CAPAB: {:?} {:?}", args, suffix);
                        },
                        unk => {
                            warn!("Received unprefixed raw message: {} {:?} {:?}", unk, args, suffix);
                        },
                    }
                },
                other => {
                    warn!("Received unprefixed message: {:?}", other);
                }
            }
            return Ok(());
        };
        match m.command {
            Command::PRIVMSG(target, msg) => {
                if Some(prefix) != self.admin_uuid() {
                    return Ok(());
                }
                if self.channels.contains(&target) {
                    self.wa_tx.unbounded_send(WhatsappCommand::SendGroupMessage(target, msg))
                        .unwrap()
                }
                else if target == self.cfg.log_chan {
                    self.process_admin_command(msg)?;
                }
                else {
                    if let Some(addr) = self.contacts_uuid_pdua.get(&target) {
                        if let Some(ct) = self.contacts.get(&addr) {
                            if ct.wa_mode {
                                self.wa_tx.unbounded_send(WhatsappCommand::SendDirectMessage(addr.clone(), msg))
                                    .unwrap();
                            }
                            else {
                                self.m_tx.unbounded_send(ModemCommand::SendMessage(addr.clone(), msg))
                                    .unwrap();
                            }
                        }
                    }
                }
            },
            Command::NOTICE(target, msg) => {
                if let Some(addr) = self.contacts_uuid_pdua.get(&target).map(|x| x.clone()) {
                    self.process_contact_admin_command(addr, msg)?;
                }
            },
            Command::ERROR(details) => {
                Err(format_err!("Error from server: {}", details))?
            },
            Command::KILL(uuid, reason) => {
                debug!("Client {} killed by {} with reason: {}", prefix, uuid, reason);
                self.remove_user(&uuid, true)?;
            },
            Command::QUIT(reason) => {
                debug!("Client {} disconnecting for reason: {:?}", prefix, reason);
                self.remove_user(&prefix, true)?;
            },
            Command::PING(dest_data, None) => {
                self.send_sid_line("PONG", vec![&dest_data], None)?;
            },
            Command::PING(source, Some(dest)) => {
                // "Summary: if you get a ping, swap the last two
                // params and send it back on the connection it came
                // from. Don't ask why, just do." 
                self.send_sid_line("PONG", vec![&dest, &source], None)?;
            },
            Command::Raw(cmd, args, suffix) => {
                match &cmd as &str {
                    "UID" => {
                        let (uuid, user) = InspUser::new_from_uid_line(args, suffix)?;
                        debug!("Received new user {}: {:?}", uuid, user);
                        self.users.insert(uuid, user);
                    },
                    "NICK" => {
                        if args.len() != 2 {
                            warn!("Invalid NICK received: {:?}", args);
                            return Ok(());
                        }
                        if let Some(user) = self.users.get_mut(&prefix) {
                            debug!("UUID {} changed nick to {}", prefix, prefix);
                            user.nick = args.into_iter().nth(0).unwrap();
                        }
                    },
                    "FHOST" => {
                        if args.len() != 1 {
                            warn!("Invalid FHOST received: {:?}", args);
                            return Ok(());
                        }
                        let host = args.into_iter().nth(0).unwrap();
                        if let Some(user) = self.users.get_mut(&prefix) {
                            debug!("UUID {} changed host to {}", prefix, host);
                            user.displayed_hostname = host;
                        }
                    },
                    "BURST" => {
                        info!("Receiving burst");
                    },
                    "ENDBURST" => {
                        info!("Remote burst complete");
                        self.state = LinkState::Linked;
                    },
                    other => {
                        debug!("Received unimplemented raw command {}: {:?} {:?}", other, args, suffix);
                    }
                }
            },
            other => {
                debug!("Received unimplemented command {:?}", other);
            }
        }
        Ok(())
    }
    fn admin_uuid(&mut self) -> Option<String> {
        for (uuid, user) in self.users.iter() {
            if user.nick == self.cfg.admin_nick {
                return Some(uuid.to_owned());
            }
        }
        None
    }
    fn process_groups(&mut self) -> Result<()> {
        let mut join = vec![];
        let mut part = vec![];
        let mut chans = vec![];
        for grp in self.store.get_all_groups()? {
            if !self.channels.contains(&grp.channel) {
                join.push(grp.channel.clone());
            }
            chans.push(grp.channel);
        }
        for ch in ::std::mem::replace(&mut self.channels, chans) {
            if !self.channels.contains(&ch) {
                part.push(ch);
            }
        }
        let mut lines = vec![];
        for ch in join {
            lines.push(Message::new(Some(&self.control_uuid), "JOIN", vec![&ch], None)?);
            for (_, contact) in self.contacts.iter() {
                lines.push(Message::new(Some(&contact.uuid), "JOIN", vec![&ch], None)?);
            }
        }
        for ch in part {
            lines.push(Message::new(Some(&self.control_uuid), "PART", vec![&ch], None)?);
            for (_, contact) in self.contacts.iter() {
                lines.push(Message::new(Some(&contact.uuid), "PART", vec![&ch], None)?);
            }
        }
        for line in lines {
            self.send(line);
        }
        Ok(())
    }
    fn contact_message(&mut self, uid: &str, kind: &str, target: &str, message: &str) -> Result<()> {
        if self.users.get(uid).is_none() {
            Err(format_err!("contact_message called with unknown uid {}", uid))?
        }
        if !uid.starts_with(&self.cfg.sid) {
            Err(format_err!("Got contact_message for uid {} that isn't owned by us", uid))?;
        }
        self.outbox.push(Message::new(Some(uid), kind, vec![target], Some(message))?);
        Ok(())
    }
    fn handle_cf_command(&mut self, cfc: ContactFactoryCommand) -> Result<()> {
        use self::ContactFactoryCommand::*;

        match cfc {
            ProcessMessages => self.process_messages()?,
            ProcessGroups => self.process_groups()?,
            MakeContact(a) => self.make_contact(a)?,
            DropContact(a) => self.drop_contact(a)?,
            LoadRecipients => {
                // don't need to do anything; recipients
                // loaded on burst
            },
            UpdateAway(a, text) => {
                if let Some(ct) = self.contacts.get(&a) {
                    self.outbox.push(Message::new(Some(&ct.uuid), "AWAY", vec![], text.as_ref().map(|x| x as &str))?);
                }
            }
        }
        Ok(())
    }
    fn handle_cb_command(&mut self, cbc: ControlBotCommand) -> Result<()> {
        use self::ControlBotCommand::*;

        match cbc {
            Log(line) => {
                let logline = Message::new(Some(&self.cfg.sid), "NOTICE", vec![&self.cfg.log_chan], Some(&line))?;
                self.send(logline);
            },
            ReportFailure(fail) => {
                if let Some(admu) = self.admin_uuid() {
                    let line = Message::new(Some(&self.cfg.sid), "NOTICE", vec![&admu], Some(&format!("\x02\x0304{}\x0f", fail)))?;
                    self.send(line);
                }
                else {
                    warn!("Unreportable failure: {}", fail);
                }
            },
            CommandResponse(resp) => {
                let line = Message::new(Some(&self.control_uuid), "PRIVMSG", vec![&self.cfg.log_chan], Some(&format!("{}: {}", self.cfg.admin_nick, resp)))?;
                self.send(line);
            },
            ProcessGroups => self.process_groups()?,
        }
        Ok(())
    }
    fn make_uid_line(&mut self, uid: &str) -> Result<Message> {
        // :<sid of new users server> UID <uid> <timestamp> <nick> <hostname> <displayed-hostname> <ident> <ip> <signon time> +<modes {mode params}> :<gecos>
        if let Some(ref user) = self.users.get(uid) {
            let msg = Message::new(
                Some(&self.cfg.sid), "UID",
                vec![&user.ts.to_string(), &user.nick, &user.hostname, 
                     &user.displayed_hostname, &user.ident, &user.ip,
                     &user.signon_time.to_string(), &user.modes],
                Some(&user.gecos)
            )?;
            Ok(msg)
        }
        else {
            Err(format_err!("no user found in make_uid_line()"))
        }
    }
    fn send(&mut self, m: Message) {
        self.outbox.push(m);
    }
    fn get_uuid(&mut self) -> Result<String> {
        if self.next_user_id > 99999 {
            Err(format_err!("UUIDs exhausted"))?
        }
        let ret = format!("{}Z{:05}", self.cfg.sid, self.next_user_id);
        self.next_user_id += 1;
        Ok(ret)
    }
    fn new_user(&mut self, user: InspUser) -> Result<String> {
        let uuid = self.get_uuid()?;
        debug!("New user {}: {:?}", uuid, user);
        self.users.insert(uuid.clone(), user);
        let line = self.make_uid_line(&uuid)?;
        self.send(line);
        Ok(uuid)
    }
    fn process_messages(&mut self) -> Result<()> {
        use huawei_modem::convert::TryFrom;

        for msg in self.store.get_all_messages()? {
            debug!("Processing message #{}", msg.id);
            let addr = util::un_normalize_address(&msg.phone_number)
                .ok_or(format_err!("invalid address {} in db", msg.phone_number))?;
            if !self.has_contact(&addr) {
                self.make_contact(addr.clone())?;
            }
            let uuid = self.contacts.get(&addr).unwrap().uuid.clone();
            if msg.pdu.is_some() {
                let pdu = DeliverPdu::try_from(msg.pdu.as_ref().unwrap())?;
                self.process_msg_pdu(&uuid, msg, pdu)?;
            }
            else {
                self.process_msg_plain(&uuid, msg)?;
            }
        }
        Ok(())
    }
    fn send_sid_line(&mut self, cmd: &str, args: Vec<&str>, suffix: Option<&str>) -> Result<()> {
        let m = Message::new(Some(&self.cfg.sid), cmd, args, suffix)?;
        self.send(m);
        Ok(())
    }
    fn do_capab_and_server(&mut self) -> Result<()> {
        info!("Sending CAPAB and SERVER lines");
        let capabs = vec![INSP_PROTOCOL_CAPAB, "NICKMAX=100", "CHANMAX=100", "MAXMODES=100", 
                          "IDENTMAX=100", "MAXQUIT=100", "MAXTOPIC=100", "MAXKICK=100",
                          "MAXGECOS=100", "MAXAWAY=100"];
        self.send_sid_line("CAPAB", vec!["START"], None)?;
        for capab in capabs {
            self.send_sid_line("CAPAB", vec!["CAPABILITIES"], Some(capab))?;
        }
        self.send_sid_line("CAPAB", vec!["END"], None)?;
        let m = Message::new(Some(&self.cfg.sid), "SERVER", vec![&self.cfg.server_name, &self.cfg.sendpass, "0", &self.cfg.sid], Some(&self.cfg.server_desc))?;
        self.send(m);
        Ok(())
    }
    fn do_burst(&mut self) -> Result<()> {
        use chrono::Utc;
        info!("Sending burst");
        let burst_ts = Utc::now().timestamp().to_string();
        self.send_sid_line("BURST", vec![&burst_ts], None)?;
        let cb = InspUser::new_for_controlbot(self.cfg.control_nick.clone());
        let uuid = self.control_uuid.clone();
        self.users.insert(uuid.clone(), cb);
        let line = self.make_uid_line(&uuid)?;
        self.send(line);
        for recip in self.store.get_all_recipients()? {
            let addr = util::un_normalize_address(&recip.phone_number)
                .ok_or(format_err!("invalid phone number in db"))?;
            let user = InspUser::new_from_recipient(addr, recip.nick);
            self.new_user(user)?;
        }
        self.send_sid_line("ENDBURST", vec![], None)?;
        Ok(())
    }
}
