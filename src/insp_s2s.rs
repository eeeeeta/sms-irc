//! InspIRCd server-to-server linking.
use tokio_core::net::TcpStream;
use tokio_codec::Framed;
use irc::proto::IrcCodec;
use irc::proto::message::Message;
use irc::proto::command::Command;
use futures::{Future, Async, Poll, Stream, Sink, AsyncSink, self};
use futures::future::Either;
use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use comm::{ControlBotCommand, ContactFactoryCommand, InitParameters, WhatsappCommand, ModemCommand};
use store::Store;
use huawei_modem::pdu::{PduAddress, DeliverPdu};
use std::collections::{HashSet, HashMap};
use failure::Error;
use models::Recipient;
use util::{self, Result};
use contact_common::ContactManagerManager;
use sender_common::Sender;
use control_common::ControlCommon;
use insp_user::InspUser;
use config::InspConfig;
use std::net::{SocketAddr, ToSocketAddrs};
use postgres::Connection as PgConn;

pub static INSP_PROTOCOL_CAPAB: &str = "PROTOCOL=1203";
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
    quassel: Option<PgConn>
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
        for msg in ::std::mem::replace(&mut self.outbox, vec![]) {
            trace!("--> {:?}", msg);
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
    fn extension_helptext() -> &'static str {
        r#"[InspIRCd s2s-specific commands]
- !raw <raw IRC line>: send a line over the wire
- !uuid <uuid>: query information about a UUID
- !uunick <nick>: get a UUID for a nick
"#
    }
    fn extension_command(&mut self, msg: Vec<&str>) -> Result<()> {
        match msg[0] {
            "!uuid" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!uuid takes an argument.")?;
                    return Ok(());
                }
                let ret = format!("{:#?}", self.users.get(msg[1]));
                for line in ret.lines() {
                    self.send_cb_message(line)?;
                }
            },
            "!raw" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!raw takes some arguments.")?;
                    return Ok(());
                }
                let m: Message = match msg[1..].join(" ").parse() {
                    Ok(m) => m,
                    Err(e) => {
                        self.send_cb_message(&format!("parse err: {}", e))?;
                        return Ok(());
                    }
                };
                self.outbox.push(m);
            },
            "!uunick" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!uunick takes an argument.")?;
                    return Ok(());
                }
                let mut nick = None;
                for (uuid, user) in self.users.iter() {
                    if user.nick == msg[1] {
                        nick = Some(uuid.clone());
                    }
                }
                self.send_cb_message(&format!("result = {:?}", nick))?;
            },
            x => self.unrecognised_command(x)?
        }
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
    fn host_for_wa(&self, wa: bool) -> String {
        if wa {
            format!("wa.{}", self.cfg.server_name)
        }
        else {
            format!("s.{}", self.cfg.server_name)
        }
    }
    pub fn new(p: InitParameters<InspConfig>, qdb: Option<PgConn>) -> impl Future<Item = Self, Error = Error> {
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
        let control_uuid = format!("{}A00000", cfg.sid);
        info!("Connecting to {}", addr);
        let fut = TcpStream::connect(&addr, p.hdl)
            .map(|res| {
                Self {
                    conn: Framed::new(res, codec),
                    cfg,
                    control_uuid,
                    next_user_id: 1,
                    cf_rx, cf_tx, cb_rx, wa_tx, m_tx,
                    users: HashMap::new(),
                    contacts: HashMap::new(),
                    contacts_uuid_pdua: HashMap::new(),
                    channel_topics: HashMap::new(),
                    store,
                    outbox: vec![],
                    channels: HashSet::new(),
                    remote_sid: "XXX".into(),
                    state: LinkState::TcpConnected,
                    quassel: qdb
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
    fn process_contact_admin_command(&mut self, addr: PduAddress, mesg: String) -> Result<()> {
        use chrono::Utc;
        debug!("processing contact admin command for {}: {}", addr, mesg);
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
                let is_wa = {
                    let mut ct = self.contacts.get(&addr).unwrap();
                    ct.wa_mode
                };
                let wa_state = !is_wa;
                self.set_wa_state(&addr, wa_state)?;
                self.contact_message(&uid, "NOTICE", &auid, &format!("WhatsApp mode: {}", if wa_state { "ENABLED" } else { "DISABLED" }))?;
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
                self.make_contact(a, false)?;
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
                if target == self.cfg.log_chan {
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
            Command::NOTICE(target, msg) => {
                if Some(prefix) != self.admin_uuid() {
                    debug!("Not admin ({:?}), returning", self.admin_uuid());
                    return Ok(());
                }
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
    fn process_avatars(&mut self) -> Result<()> {
        let mut processed = 0;
        let qdb = match self.quassel {
            Some(ref mut qdb) => qdb,
            None => return Ok(())
        };
        for recip in self.store.get_all_recipients()? {
            let addr = util::un_normalize_address(&recip.phone_number)
                .ok_or(format_err!("invalid phone number in db"))?;
            if let Some(ic) = self.contacts.get(&addr) {
                if let Some(iu) = self.users.get(&ic.uuid) {
                    let query = format!("{}!{}@%.{}", iu.nick, iu.gecos, self.cfg.server_name);
                    let res = qdb.execute("UPDATE sender SET avatarurl = $1 WHERE sender LIKE $2",
                                          &[&recip.avatar_url, &query]);
                    match res {
                        Ok(p) => {
                            processed += p;
                        },
                        Err(e) => {
                            warn!("Failed to update avatar for {}: {}", iu.nick, e);
                        }
                    }
                }
                else {
                    warn!("avatars: no user for contact {}", addr);
                }
            }
            else {
                warn!("avatars: no contact for address {}", addr);
            }
        }
        info!("Updated {} Quassel avatar entries.", processed);
        Ok(())
    }
    fn handle_cf_command(&mut self, cfc: ContactFactoryCommand) -> Result<()> {
        use self::ContactFactoryCommand::*;

        match cfc {
            ProcessMessages => self.process_messages()?,
            ProcessGroups => self.process_groups()?,
            MakeContact(a, wa) => self.make_contact(a, wa)?,
            DropContact(a) => self.drop_contact(a)?,
            LoadRecipients => {
                // don't need to do anything; recipients
                // loaded on burst
            },
            UpdateAway(a, text) => {
                if let Some(ct) = self.contacts.get(&a) {
                    self.outbox.push(Message::new(Some(&ct.uuid), "AWAY", vec![], text.as_ref().map(|x| x as &str))?);
                }
            },
            ProcessAvatars => self.process_avatars()?
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
        use huawei_modem::convert::TryFrom;

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
                self.make_contact(addr.clone(), msg.text.is_some())?;
            }
            let (uuid, is_wa) = {
                let ct = self.contacts.get(&addr).unwrap();
                (ct.uuid.clone(), ct.wa_mode)
            };
            if msg.pdu.is_some() {
                let pdu = DeliverPdu::try_from(msg.pdu.as_ref().unwrap())?;
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
        let cb = InspUser::new_for_controlbot(self.cfg.control_nick.clone(), &self.cfg.server_name);
        let uuid = self.control_uuid.clone();
        self.users.insert(uuid.clone(), cb);
        let line = self.make_uid_line(&uuid)?;
        self.send(line);
        for recip in self.store.get_all_recipients()? {
            let addr = util::un_normalize_address(&recip.phone_number)
               .ok_or(format_err!("invalid phone number in db"))?;
            self.make_contact(addr, recip.whatsapp)?;
        }
        self.send_sid_line("ENDBURST", vec![], None)?;
        Ok(())
    }
}
