//! InspIRCd server-to-server linking.
use tokio_core::net::TcpStream;
use tokio_codec::Framed;
use irc::proto::IrcCodec;
use irc::proto::message::Message;
use irc::proto::command::Command;
use futures::{Future, Async, Poll, Stream, Sink, self};
use futures::future::Either;
use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use crate::comm::{ControlBotCommand, ContactFactoryCommand, InitParameters, WhatsappCommand, ModemCommand, ContactManagerCommand};
use crate::store::Store;
use huawei_modem::pdu::{PduAddress, DeliverPdu};
use std::collections::{HashSet, HashMap};
use failure::Error;
use crate::models::Recipient;
use crate::util::{self, Result};
use crate::contact_common::ContactManagerManager;
use crate::sender_common::Sender;
use crate::control_common::ControlCommon;
use crate::insp_user::InspUser;
use crate::config::InspConfig;
use crate::admin::InspCommand;
use std::net::{SocketAddr, ToSocketAddrs};

pub static INSP_PROTOCOL_CAPAB: &str = "PROTOCOL=1202";
pub static INSP_PROTOCOL_VERSION: &str = "1202";

struct InspContact {
    uuid: String,
    channels: Vec<String>,
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
    remote_sid: String,
    cf_rx: UnboundedReceiver<ContactFactoryCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    cb_rx: UnboundedReceiver<ControlBotCommand>,
    // Okay, this is a bit silly, but hey, standardization...
    cb_tx: UnboundedSender<ControlBotCommand>,
    wa_tx: UnboundedSender<WhatsappCommand>,
    m_tx: UnboundedSender<ModemCommand>,
    // map of UID -> InspUser
    users: HashMap<String, InspUser>,
    contacts: HashMap<PduAddress, InspContact>,
    contacts_uuid_pdua: HashMap<String, PduAddress>,
    channel_topics: HashMap<String, String>,
    store: Store,
    outbox: Vec<Message>,
    channels: HashSet<String>,
    state: LinkState,
}

impl Future for InspLink {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        if self.state == LinkState::TcpConnected {
            self.do_capab_and_server()?;
            self.state = LinkState::ServerSent;
        }
        while let Async::Ready(msg) = self.conn.poll()? {
            let msg = msg.ok_or(format_err!("disconnected"))?;
            trace!("<-- {}", msg);
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
        sink_outbox!(self, outbox, conn, "");
        Ok(Async::NotReady)
    }
}
impl ContactManagerManager for InspLink {
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand> { &mut self.wa_tx }
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand> { &mut self.m_tx }
    fn cb_tx(&mut self) -> &mut UnboundedSender<ControlBotCommand> { &mut self.cb_tx }
    fn setup_contact_for(&mut self, recip: Recipient, addr: PduAddress) -> Result<()> {
        trace!("setting up contact for recip #{}: {}", recip.id, addr);
        let host = self.host_for_wa(recip.whatsapp);
        let user = InspUser::new_from_recipient(addr.clone(), recip.nick, &host);
        let uuid = self.new_user(user)?;
        self.contacts.insert(addr.clone(), InspContact {
            uuid: uuid.clone(),
            channels: vec![],
            wa_mode: recip.whatsapp
        });
        self.contacts_uuid_pdua.insert(uuid, addr.clone());
        if self.state == LinkState::Linked {
            self.process_groups_for_recipient(&addr)?;
        }
        Ok(())
    }
    fn remove_contact_for(&mut self, addr: &PduAddress) -> Result<()> {
        trace!("removing contact for {}", addr);
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
    fn resolve_nick(&self, nick: &str) -> Option<PduAddress> {
        for (uuid, user) in self.users.iter() {
            if user.nick == nick {
                if let Some(pdua) = self.contacts_uuid_pdua.get(uuid) {
                    return Some(pdua.clone());
                }
            }
        }
        None
    }
    fn forward_cmd(&mut self, a: &PduAddress, cmd: ContactManagerCommand) -> Result<()> {
        if let Some(ct) = self.contacts.get_mut(&a) {
            match cmd {
                ContactManagerCommand::UpdateAway(text) => {
                    self.outbox.push(Message::new(Some(&ct.uuid), "AWAY", vec![], text.as_ref().map(|x| x as &str))?);
                },
                ContactManagerCommand::ChangeNick(st) => {
                    let u = self.users.get_mut(&ct.uuid).unwrap();
                    u.nick = st.clone();
                    let ts = chrono::Utc::now().timestamp().to_string();
                    self.outbox.push(Message::new(Some(&ct.uuid), "NICK", vec![&st, &ts], None)?);
                    self.store.update_recipient_nick(&a, &st)?;
                },
                ContactManagerCommand::SetWhatsapp(wam) => {
                    ct.wa_mode = wam;
                    self.set_wa_state(&a, wam)?;
                },
                _ => {}
            }
        }
        else {
            debug!("Failed to forward command to nonexistent ghost with address {}", a);
        }
        Ok(())
    }
}
impl ControlCommon for InspLink {
    fn cf_tx(&mut self) -> &mut UnboundedSender<ContactFactoryCommand> { &mut self.cf_tx }
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand> { &mut self.wa_tx }
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand> { &mut self.m_tx }
    fn control_response(&mut self, msg: &str) -> Result<()> {
        if let Some(admu) = self.admin_uuid() {
            let line = Message::new(Some(&self.control_uuid), "NOTICE", vec![&admu], Some(msg))?;
            self.send(line);
        }
        else {
            warn!("Control response dropped: {}", msg);
        }
        Ok(())
    }
    fn process_insp(&mut self, ic: InspCommand) -> Result<bool> {
        use self::InspCommand::*;
        match ic {
            Raw(msg) => {
                let m: Message = match msg.parse() {
                    Ok(m) => m,
                    Err(e) => {
                        self.control_response(&format!("parse err: {}", e))?;
                        return Ok(true);
                    }
                };
                self.outbox.push(m);
            },
            QueryUuid(uu) => {
                let ret = format!("{:#?}", self.users.get(&uu));
                for line in ret.lines() {
                    self.control_response(line)?;
                }
            },
            QueryNickUuid(qnu) => {
                let mut nick = None;
                for (uuid, user) in self.users.iter() {
                    if user.nick == qnu {
                        nick = Some(uuid.clone());
                    }
                }
                self.control_response(&format!("result = {:?}", nick))?;
            }
        }
        Ok(true)
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
    fn ensure_joined(&mut self, ch: &str) -> Result<()> {
        if !self.cfg.ensure_joined {
            return Ok(());
        }
        if let Some(admu) = self.admin_uuid() {
            self.send_sid_line("SVSJOIN", vec![&admu, ch], None)?;
        }
        Ok(())
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
    fn host_for_wa(&self, wa: bool) -> String {
        if wa {
            format!("wa.{}", self.cfg.server_name)
        }
        else {
            format!("s.{}", self.cfg.server_name)
        }
    }
    pub fn new(p: InitParameters<InspConfig>) -> impl Future<Item = Self, Error = Error> {
        let store = p.store;
        let cfg = p.cfg2.clone();
        let cf_rx = p.cm.cf_rx.take().unwrap();
        let cb_rx = p.cm.cb_rx.take().unwrap();
        let cf_tx = p.cm.cf_tx.clone();
        let cb_tx = p.cm.cb_tx.clone();
        let wa_tx = p.cm.wa_tx.clone();
        let m_tx = p.cm.modem_tx.clone();
        let (addr, codec) = match Self::_make_addr_and_codec(&cfg) {
            Ok(x) => x,
            Err(e) => return Either::B(futures::future::err(e))
        };
        let control_uuid = format!("{}A00000", cfg.sid);
        info!("Connecting to {}", addr);
        let fut = TcpStream::connect(&addr, p.hdl)
            .map(|res| {
                Self {
                    conn: Framed::new(res, codec),
                    cfg,
                    control_uuid,
                    next_user_id: 1,
                    cf_rx, cf_tx, cb_rx, cb_tx, wa_tx, m_tx,
                    users: HashMap::new(),
                    contacts: HashMap::new(),
                    contacts_uuid_pdua: HashMap::new(),
                    channel_topics: HashMap::new(),
                    store,
                    outbox: vec![],
                    channels: HashSet::new(),
                    remote_sid: "XXX".into(),
                    state: LinkState::TcpConnected,
                }
            })
            .map_err(|e| e.into());
        Either::A(fut)
    }
    fn set_wa_state(&mut self, addr: &PduAddress, wa_state: bool) -> Result<()> {
        let (uid, is_wa) = {
            let mut ct = self.contacts.get_mut(&addr).unwrap();
            ct.wa_mode = wa_state;
            self.store.update_recipient_wa(&addr, ct.wa_mode)?;
            (ct.uuid.clone(), ct.wa_mode)
        };
        let host = self.host_for_wa(is_wa);
        self.outbox.push(Message::new(Some(&uid), "FHOST", vec![&host], None)?);
        if let Some(user) = self.users.get_mut(&uid) {
            user.displayed_hostname = host;
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
                self.setup_contact(a)?;
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
                            info!("Verified server: {} [{}] ({})", args[0], args[3], suffix.unwrap());
                            self.remote_sid = args[3].to_owned();
                            self.do_burst()?;
                            self.state = LinkState::BurstSent;
                        },
                        "CAPAB" => {
                            debug!("Received CAPAB: {:?} {:?}", args, suffix);
                        },
                        " " | "" => {},
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
                    debug!("Not admin ({:?}), returning", self.admin_uuid());
                    return Ok(());
                }
                if target == self.control_uuid {
                    self.process_admin_command(msg)?;
                }
                else if self.channels.contains(&target) {
                    debug!("Sending WA group message for {}", target);
                    self.wa_tx.unbounded_send(WhatsappCommand::SendGroupMessage(target, msg))
                        .unwrap()
                }
                else {
                    if let Some(addr) = self.contacts_uuid_pdua.get(&target) {
                        if let Some(ct) = self.contacts.get(&addr) {
                            if ct.wa_mode {
                                debug!("Sending WA DM for {}", target);
                                self.wa_tx.unbounded_send(WhatsappCommand::SendDirectMessage(addr.clone(), msg))
                                    .unwrap();
                            }
                            else {
                                debug!("Sending modem DM for {}", target);
                                self.m_tx.unbounded_send(ModemCommand::SendMessage(addr.clone(), msg))
                                    .unwrap();
                            }
                        }
                    }
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
            Command::SQUIT(server, reason) => {
                let mut split = 0;
                self.users.retain(|k, _| {
                    let ret = k.starts_with(&server);
                    if ret {
                        split += 1;
                    }
                    !ret
                });
                warn!("SQUIT of {} by {} ({} users split): {}", server, prefix, split, reason);
                if self.admin_uuid().is_none() {
                    warn!("(admin lost in SQUIT)");
                }
            },
            Command::Raw(cmd, args, suffix) => {
                match &cmd as &str {
                    "UID" => {
                        let (uuid, user) = InspUser::new_from_uid_line(args, suffix)?;
                        debug!("Received new user {}: {:?}", uuid, user);
                        let is_admin = user.nick == self.cfg.admin_nick;
                        self.users.insert(uuid, user);
                        if is_admin {
                            info!("Admin has returned; processing messages");
                            self.process_messages()?;
                        }
                    },
                    "NICK" => {
                        if args.len() < 1 {
                            warn!("Invalid NICK received: {:?}", args);
                            return Ok(());
                        }
                        if let Some(user) = self.users.get_mut(&prefix) {
                            debug!("UUID {} changed nick to {}", prefix, prefix);
                            user.nick = args.into_iter().nth(0).unwrap();
                        }
                    },
                    "FTOPIC" => {
                        if args.len() < 3 || suffix.is_none() {
                            warn!("Invalid FTOPIC received: {:?}, {:?}", args, suffix);
                            return Ok(());
                        }
                        let topic = suffix.unwrap();
                        debug!("Topic for {} is: {}", args[0], topic);
                        self.channel_topics.insert(args[0].to_string(), topic);
                    },
                    "FHOST" => {
                        if args.len() < 1 {
                            // These are pretty common, for some reason.
                            debug!("Invalid FHOST received: {:?}", args);
                            return Ok(());
                        }
                        let host = args.into_iter().nth(0).unwrap();
                        if let Some(user) = self.users.get_mut(&prefix) {
                            debug!("UUID {} changed host to {}", prefix, host);
                            user.displayed_hostname = host;
                        }
                    },
                    "BURST" => {
                        debug!("Receiving burst");
                    },
                    "ENDBURST" => {
                        if self.remote_sid == prefix {
                            debug!("Received end of netburst");
                            self.state = LinkState::Linked;
                            self.on_linked()?;
                        }
                        else {
                            debug!("Server {} finished bursting", prefix);
                        }
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
    fn on_linked(&mut self) -> Result<()> {
        info!("Link established to remote server.");
        self.outbox.push(Message::new(Some(&self.control_uuid), "JOIN", vec![&self.cfg.log_chan], None)?);
        self.process_groups()?;
        self.process_messages()?;
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
    fn process_groups_for_recipient(&mut self, a: &PduAddress) -> Result<()> {
        // FIXME: It becomes delicious copypasta, they shall eat it.

        debug!("Processing group changes for {}", a);
        let ct = self.contacts.get_mut(a)
            .expect("invalid addr in pgfr()");
        let mut chans = vec![];
        for grp in self.store.get_groups_for_recipient(a)? {
            debug!("{} joining {}", ct.uuid, grp.channel);
            self.outbox.push(Message::new(Some(&ct.uuid), "JOIN", vec![&grp.channel], None)?);
            chans.push(grp.channel.clone());
            self.channels.insert(grp.channel);
        }
        if !chans.contains(&self.cfg.log_chan) {
            self.outbox.push(Message::new(Some(&ct.uuid), "JOIN", vec![&self.cfg.log_chan], None)?);
            chans.push(self.cfg.log_chan.clone());
        }
        for ch in ::std::mem::replace(&mut ct.channels, chans) {
            if !ct.channels.contains(&ch) {
                debug!("{} parting {}", ct.uuid, ch);
                self.outbox.push(Message::new(Some(&ct.uuid), "PART", vec![&ch], Some("Left group"))?);
            }
        }
        debug!("{} channels now: {:?}", a, ct.channels);
        Ok(())
    }
    fn process_groups(&mut self) -> Result<()> {
        self.channels = HashSet::new();
        for addr in self.contacts.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>() {
            self.process_groups_for_recipient(&addr)?;
        }
        for grp in self.store.get_all_groups()? {
            for part in grp.participants {
                if let Some(recip) = self.store.get_recipient_by_id_opt(part)? {
                    let num = util::un_normalize_address(&recip.phone_number)
                        .ok_or(format_err!("invalid address {} in db", recip.phone_number))?;
                    if let Some(ct) = self.contacts.get(&num) {
                        let mode = if grp.admins.contains(&part) {
                            "+o"
                        }
                        else {
                            "-o"
                        };
                        self.outbox.push(Message::new(Some(&self.cfg.sid), "MODE", vec![&grp.channel, mode, &ct.uuid], None)?);
                    }
                }
            }
            if self.cfg.set_topics {
                // FIXME: this clone isn't nice :(
                let topic = self.channel_topics.entry(grp.channel.clone()).or_insert("".into());
                if topic == "" || self.cfg.clobber_topics && topic as &str != grp.topic {
                    // doing this makes Atheme angry.
                    self.outbox.push(Message::new(Some(&self.cfg.sid), "TOPIC", vec![&grp.channel], Some(&grp.topic))?);
                    *topic = grp.topic;
                }
            }
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
            SetupContact(a) => self.setup_contact(a)?,
            QueryContact(a, src) => self.query_contact(a, src)?,
            DropContact(a) => self.drop_contact(a)?,
            DropContactByNick(a) => self.drop_contact_by_nick(a)?,
            LoadRecipients => {
                // don't need to do anything; recipients
                // loaded on burst
            },
            ForwardCommand(a, cmd) => self.forward_cmd(&a, cmd)?,
            ForwardCommandByNick(a, cmd) => self.forward_cmd_by_nick(&a, cmd)?,
            ProcessAvatars => {
                // FIXME: implement
                //
                // We'd need IRCv3 METADATA or something for this.
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
                    let line = Message::new(Some(&self.cfg.sid), "PRIVMSG", vec![&admu], Some(&format!("{}: \x02\x0304{}\x0f", self.cfg.admin_nick, fail)))?;
                    self.send(line);
                }
                else {
                    warn!("Unreportable failure: {}", fail);
                }
            },
            CommandResponse(resp) => {
                self.control_response(&resp)?;
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
                vec![uid, &user.ts.to_string(), &user.nick, &user.hostname, 
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
        use std::convert::TryFrom;

        let auid = match self.admin_uuid() {
            Some(x) => x,
            None => {
                warn!("Not processing messages; admin not connected");
                return Ok(());
            }
        };
        for msg in self.store.get_all_messages()? {
            debug!("Processing message #{}", msg.id);
            let addr = util::un_normalize_address(&msg.phone_number)
                .ok_or(format_err!("invalid address {} in db", msg.phone_number))?;
            if !self.has_contact(&addr) {
                if !self.request_contact(addr.clone(), msg.source)? {
                    continue;
                }
            }
            let (uuid, is_wa) = {
                let ct = self.contacts.get(&addr).unwrap();
                (ct.uuid.clone(), ct.wa_mode)
            };
            if msg.pdu.is_some() {
                let pdu = DeliverPdu::try_from(msg.pdu.as_ref().unwrap() as &[u8])?;
                if is_wa {
                    self.set_wa_state(&addr, false)?;
                    self.contact_message(&uuid, "NOTICE", &auid, "Notice: SMS mode automatically enabled.")?;
                }
                self.process_msg_pdu(&uuid, msg, pdu)?;
            }
            else {
                if !is_wa {
                    self.set_wa_state(&addr, true)?;
                    self.contact_message(&uuid, "NOTICE", &auid, "Notice: WhatsApp mode automatically enabled.")?;
                }
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
        debug!("Sending CAPAB and SERVER lines");
        let capabs = vec![INSP_PROTOCOL_CAPAB, "NICKMAX=100", "CHANMAX=100", "MAXMODES=100", 
                          "IDENTMAX=100", "MAXQUIT=100", "MAXTOPIC=100", "MAXKICK=100",
                          "MAXGECOS=100", "MAXAWAY=100"];
        self.send_sid_line("CAPAB", vec!["START", INSP_PROTOCOL_VERSION], None)?;
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
        let cb = InspUser::new_for_controlbot(self.cfg.control_nick.clone(), &self.cfg.server_name);
        let uuid = self.control_uuid.clone();
        self.users.insert(uuid.clone(), cb);
        let line = self.make_uid_line(&uuid)?;
        self.send(line);
        for recip in self.store.get_all_recipients()? {
            self.setup_recipient(recip)?;
        }
        self.send_sid_line("ENDBURST", vec![], None)?;
        Ok(())
    }
}
