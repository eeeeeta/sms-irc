//! Experimental support fr WhatsApp.

use whatsappweb::connection::{WhatsappWebConnection, WhatsappWebHandler};
use whatsappweb::connection::State as WaState;
use whatsappweb::Jid;
use whatsappweb::Contact as WaContact;
use whatsappweb::Chat as WaChat;
use whatsappweb::GroupMetadata;
use whatsappweb::connection::UserData as WaUserData;
use whatsappweb::connection::PersistentSession as WaPersistentSession;
use whatsappweb::connection::DisconnectReason as WaDisconnectReason;
use whatsappweb::message::ChatMessage as WaMessage;
use whatsappweb::message::{ChatMessageContent, MessageId, Peer};
use huawei_modem::pdu::PduAddress;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use std::collections::HashMap;
use store::Store;
use std::sync::Arc;
use comm::{WhatsappCommand, ContactFactoryCommand, ControlBotCommand, InitParameters};
use util::{self, Result};
use image::Luma;
use qrcode::QrCode;
use futures::{Future, Async, Poll, Stream};
use failure::Error;
use whatsapp_media::{MediaInfo, MediaResult};

struct WhatsappHandler {
    tx: Arc<UnboundedSender<WhatsappCommand>>
}
impl WhatsappWebHandler for WhatsappHandler {
    fn on_state_changed(&self, _: &WhatsappWebConnection<Self>, state: WaState) {
        self.tx.unbounded_send(WhatsappCommand::StateChanged(state))
            .unwrap();
    }
    fn on_user_data_changed(&self, _: &WhatsappWebConnection<Self>, user_data: WaUserData) {
        self.tx.unbounded_send(WhatsappCommand::UserDataChanged(user_data))
            .unwrap();
    }
    fn on_persistent_session_data_changed(&self, ps: WaPersistentSession) {
        self.tx.unbounded_send(WhatsappCommand::PersistentChanged(ps))
            .unwrap();
    }
    fn on_disconnect(&self, reason: WaDisconnectReason) {
        self.tx.unbounded_send(WhatsappCommand::Disconnect(reason))
            .unwrap();
    }
    fn on_message(&self, _: &WhatsappWebConnection<Self>, new: bool, message: Box<WaMessage>) {
        self.tx.unbounded_send(WhatsappCommand::Message(new, message))
            .unwrap();
    }
}
pub struct WhatsappManager {
    conn: Option<WhatsappWebConnection<WhatsappHandler>>,
    rx: UnboundedReceiver<WhatsappCommand>,
    wa_tx: Arc<UnboundedSender<WhatsappCommand>>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    cb_tx: UnboundedSender<ControlBotCommand>,
    contacts: HashMap<Jid, WaContact>,
    chats: HashMap<Jid, WaChat>,
    groups: HashMap<Jid, GroupMetadata>,
    pending_associations: HashMap<Jid, String>,
    state: WaState,
    connected: bool,
    store: Store,
    qr_path: String,
    media_path: String,
    dl_path: String,
    our_jid: Option<Jid>
}
impl Future for WhatsappManager {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(com) = self.rx.poll().unwrap() {
            let com = com.ok_or(format_err!("whatsappmanager rx died"))?;
            self.handle_int_rx(com)?;
        }
        Ok(Async::NotReady)
    }
}
impl WhatsappManager {
    pub fn new<T>(p: InitParameters<T>) -> Self {
        let store = p.store;
        let wa_tx = p.cm.wa_tx.clone();
        let rx = p.cm.wa_rx.take().unwrap();
        let cf_tx = p.cm.cf_tx.clone();
        let cb_tx = p.cm.cb_tx.clone();
        let qr_path = p.cfg.qr_path.clone().unwrap_or("/tmp/wa_qr.png".into());
        let media_path = p.cfg.media_path.clone().unwrap_or("/tmp/wa_media".into());
        let dl_path = p.cfg.dl_path.clone().unwrap_or("file:///tmp/wa_media".into());
        wa_tx.unbounded_send(WhatsappCommand::LogonIfSaved)
            .unwrap();
        Self {
            conn: None,
            contacts: HashMap::new(),
            chats: HashMap::new(),
            groups: HashMap::new(),
            pending_associations: HashMap::new(),
            state: WaState::Uninitialized,
            connected: false,
            our_jid: None,
            wa_tx: Arc::new(wa_tx),
            rx, cf_tx, cb_tx, qr_path, store, media_path, dl_path
        }
    }
    fn handle_int_rx(&mut self, c: WhatsappCommand) -> Result<()> {
        use self::WhatsappCommand::*;

        match c {
            StartRegistration => self.start_registration()?,
            LogonIfSaved => self.logon_if_saved()?,
            QrCode(qr) => self.on_qr(qr)?,
            SendGroupMessage(to, cont) => self.send_group_message(to, cont)?,
            SendDirectMessage(to, cont) => self.send_direct_message(to, cont)?,
            GroupAssociate(jid, to) => self.group_associate(jid, to)?,
            GroupList => self.group_list()?,
            GroupRemove(grp) => self.group_remove(grp)?,
            StateChanged(was) => self.on_state_changed(was),
            UserDataChanged(wau) => self.on_user_data_changed(wau),
            PersistentChanged(wap) => self.on_persistent_session_data_changed(wap)?,
            Disconnect(war) => self.on_disconnect(war),
            GotGroupMetadata(meta) => self.on_got_group_metadata(meta)?,
            Message(new, msg) => {
                if new {
                    self.on_message(msg)?;
                }
            },
            MediaFinished(r) => self.media_finished(r)?
        }
        Ok(())
    }
    fn media_finished(&mut self, r: MediaResult) -> Result<()> {
        match r.result {
            Ok(ret) => {
                debug!("Media download/decryption job for {} / mid {:?} complete.", r.addr, r.mi);
                self.store.store_plain_message(&r.addr, &ret, r.group)?;
                self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                    .unwrap();
                if let Some(ref mut conn) = self.conn {
                    if let Some(p) = r.peer {
                        conn.send_message_read(r.mi, p);
                    }
                }
            },
            Err(e) => {
                warn!("Decryption job failed for {} / mid {:?}: {}", r.addr, r.mi, e);
                let msg = "\x01ACTION uploaded media (couldn't download)\x01";
                self.store.store_plain_message(&r.addr, &msg, r.group)?;
                self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                    .unwrap();
            }
        }
        Ok(())
    }
    fn logon_if_saved(&mut self) -> Result<()> {
        use whatsappweb::connection;

        if let Some(wap) = self.store.get_wa_persistence_opt()? {
            info!("Logging on to WhatsApp Web using stored persistence data");
            let tx = self.wa_tx.clone();
            let (conn, _) = connection::with_persistent_session(wap, WhatsappHandler { tx }); 
            self.conn = Some(conn);
        }
        else {
            info!("WhatsApp is not configured.");
        }
        Ok(())
    }
    fn start_registration(&mut self) -> Result<()> {
        use whatsappweb::connection;

        info!("Beginning WhatsApp Web registration process");
        let tx = self.wa_tx.clone();
        let tx2 = self.wa_tx.clone();
        let (conn, _) = connection::new(move |qr| {
            tx2.unbounded_send(WhatsappCommand::QrCode(qr))
                .unwrap()
        }, WhatsappHandler { tx });
        self.conn = Some(conn);
        Ok(())
    }
    fn cb_respond(&mut self, s: String) {
        self.cb_tx.unbounded_send(ControlBotCommand::CommandResponse(s))
            .unwrap();
    }
    fn on_qr(&mut self, qr: QrCode) -> Result<()> {
        info!("Processing registration QR code...");
        qr.render::<Luma<u8>>()
            .module_dimensions(10, 10)
            .build()
            .save(&self.qr_path)?;
        let qrn = format!("Scan the QR code saved at {} to log in!", self.qr_path);
        self.cb_respond(qrn);
        self.cb_respond(format!("NB: The code is only valid for a few seconds, so scan quickly!"));
        Ok(())
    }
    fn send_direct_message(&mut self, addr: PduAddress, content: String) -> Result<()> {
        debug!("Sending direct message to {}...", addr);
        trace!("Message contents: {}", content);
        if self.conn.is_none() || !self.connected {
            warn!("Tried to send WA message to {} while disconnected!", addr);
            self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(format!("Failed to send to WA contact {}: disconnected from server", addr)))
                .unwrap();
            return Ok(());
        }
        match Jid::from_phonenumber(format!("{}", addr)) {
            Ok(jid) => {
                let content = ChatMessageContent::Text(content);
                self.conn.as_mut().unwrap()
                    .send_message(content, jid);
                debug!("WA direct message sent (probably)");
            },
            Err(e) => {
                warn!("Couldn't send WA message to {}: {}", addr, e);
                self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(format!("Failed to send to WA contact {}: {}", addr, e)))
                    .unwrap();
            }
        }
        Ok(())
    }
    fn send_group_message(&mut self, chan: String, content: String) -> Result<()> {
        debug!("Sending message to group with chan {}...", chan);
        trace!("Message contents: {}", content);
        if self.conn.is_none() || !self.connected {
            warn!("Tried to send WA message to group {} while disconnected!", chan);
            self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(format!("Failed to send to group {}: disconnected from server", chan)))
                .unwrap();
            return Ok(());
        }
        if let Some(grp) = self.store.get_group_by_chan_opt(&chan)? {
            let jid = grp.jid.parse().expect("bad jid in DB");
            let content = ChatMessageContent::Text(content);
            self.conn.as_mut().unwrap()
                .send_message(content, jid);
            debug!("WA group message sent (probably)");
        }
        else {
            error!("Tried to send WA message to nonexistent group {}", chan);
        }
        Ok(())
    }
    fn on_message(&mut self, msg: Box<WaMessage>) -> Result<()> {
        use whatsappweb::message::{Direction, Peer};

        trace!("processing WA message: {:?}", msg);
        let msg = *msg; // otherwise stupid borrowck gets angry, because Box
        let WaMessage { direction, content, id, .. } = msg;
        debug!("got message from dir {:?}", direction);
        let mut peer = None;
        let (from, group) = match direction {
            Direction::Sending(jid) => {
                let ojid = self.our_jid.clone()
                    .ok_or(format_err!("our_jid empty"))?;
                let group = if jid.is_group {
                    Some(jid)
                }
                else {
                    None
                };
                (ojid, group) 
            },
            Direction::Receiving(p) => {
                peer = Some(p.clone());
                match p {
                    Peer::Individual(j) => (j, None),
                    Peer::Group { group, participant } => (participant, Some(group))
                }
            }
        };
        let group = match group {
            Some(gid) => {
                if let Some(grp) = self.store.get_group_by_jid_opt(&gid)? {
                    Some(grp.id)
                }
                else {
                    info!("Received message for unbridged group {}, ignoring...", gid.to_string());
                    return Ok(());
                }
            },
            None => None
        };
        let mut is_media = false;
        let text = match content {
            ChatMessageContent::Text(s) => s,
            ChatMessageContent::Unimplemented(det) => format!("[\x02\x0304unimplemented\x0f] {}", det),
            mut x @ ChatMessageContent::Image(..) |
                mut x @ ChatMessageContent::Video(..) |
                mut x @ ChatMessageContent::Audio(..) |
                mut x @ ChatMessageContent::Document(..) => {
                    let capt = x.take_caption();
                    if let Some(addr) = util::jid_to_address(&from) {
                        self.process_media(id.clone(), peer.clone(), addr, group, x)?;
                        is_media = true;
                    }
                    else {
                        warn!("couldn't make address for jid {}", from.to_string());
                        return Ok(());
                    }
                    if let Some(c) = capt {
                        c
                    }
                    else {
                        return Ok(());
                    }
                }
        };
        if let Some(addr) = util::jid_to_address(&from) {
            self.store.store_plain_message(&addr, &text, group)?;
            self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                .unwrap();
        }
        else {
            warn!("couldn't make address for jid {}", from.to_string());
        }
        if let Some(p) = peer {
            if let Some(ref mut conn) = self.conn {
                if !is_media {
                    conn.send_message_read(id, p);
                }
            }
        }
        Ok(())
    }
    fn process_media(&mut self, id: MessageId, peer: Option<Peer>, addr: PduAddress, group: Option<i32>, ct: ChatMessageContent) -> Result<()> {
        use whatsappweb::MediaType;

        let (ty, fi, name) = match ct {
            ChatMessageContent::Image(fi, ..) => (MediaType::Image, fi, None),
            ChatMessageContent::Video(fi, ..) => (MediaType::Video, fi, None),
            ChatMessageContent::Audio(fi, ..) => (MediaType::Audio, fi, None),
            ChatMessageContent::Document(fi, name) => (MediaType::Document, fi, Some(name)),
            _ => unreachable!()
        };
        let mi = MediaInfo {
            ty, fi, name, peer,
            mi: id,
            addr, group,
            path: self.media_path.clone(),
            dl_path: self.dl_path.clone(),
            tx: self.wa_tx.clone()
        };
        mi.start();
        Ok(())
    }
    fn group_list(&mut self) -> Result<()> {
        let mut list = vec![];
        for (jid, gmeta) in self.chats.iter() {
            let bstatus = if let Some(grp) = self.store.get_group_by_jid_opt(jid)? {
                format!("\x02\x0309group bridged to {}\x0f", grp.channel)
            }
            else {
                if jid.is_group {
                    format!("\x02\x0304unbridged group\x0f")
                }
                else {
                    format!("\x021-to-1 chat\x0f")
                }
            };
            list.push(format!("- '{}' (jid {}) - {}", gmeta.name.as_ref().map(|x| x as &str).unwrap_or("<unnamed>"), jid.to_string(), bstatus));
        }
        if list.len() == 0 {
            self.cb_respond("no WhatsApp chats (yet?)".into());
        }
        else {
            self.cb_respond("WhatsApp chats:".into());
        }
        for item in list {
            self.cb_respond(item);
        }
        Ok(())
    }
    fn on_got_group_metadata(&mut self, meta: GroupMetadata) -> Result<()> {
        info!("Got metadata for group '{}' (jid {})", meta.subject, meta.id.to_string());
        let id = meta.id.clone();
        self.groups.insert(meta.id.clone(), meta);
        if let Some(chan) = self.pending_associations.remove(&id) {
            info!("Executing pending association with chan {}", chan);
            self.group_associate(id, chan)?;
        }
        Ok(())
    }
    fn group_associate(&mut self, jid: Jid, chan: String) -> Result<()> {
        if let Some(grp) = self.store.get_group_by_jid_opt(&jid)? {
            self.cb_respond(format!("that group already exists (channel {})!", grp.channel));
            return Ok(());
        }
        if let Some(grp) = self.store.get_group_by_chan_opt(&chan)? {
            self.cb_respond(format!("that channel is already used for a group (jid {})!", grp.jid));
            return Ok(());
        }
        if self.conn.is_none() || !self.connected {
            self.cb_respond("we aren't connected to WhatsApp!".into());
            return Ok(());
        }
        if !jid.is_group {
            self.cb_respond("that jid isn't a group!".into());
            return Ok(());
        }
        if self.groups.get(&jid).is_none() {
            info!("Getting metadata for jid {}", jid.to_string());
            self.pending_associations.insert(jid.clone(), chan);
            let tx = self.wa_tx.clone();
            self.conn.as_mut().unwrap()
                .get_group_metadata(&jid, Box::new(move |m| {
                    if let Some(m) = m {
                        tx.unbounded_send(WhatsappCommand::GotGroupMetadata(m))
                            .unwrap();
                    }
                    else {
                        warn!("Got empty group metadata, for some reason");
                    }
                }));
            return Ok(());
        }
        info!("Creating new group for jid {}", jid.to_string());
        let grp = {
            let grp = self.groups.get(&jid).unwrap();
            let mut participants = vec![];
            let mut admins = vec![];
            for &(ref jid, admin) in grp.participants.iter() {
                if let Some(addr) = util::jid_to_address(jid) {
                    let recip = if let Some(recip) = self.store.get_recipient_by_addr_opt(&addr)? {
                        recip
                    }
                    else {
                        let mut nick = util::make_nick_for_address(&addr);
                        if let Some(ct) = self.contacts.get(jid) {
                            if let Some(ref name) = ct.name {
                                nick = util::string_to_irc_nick(name);
                            }
                            else if let Some(ref name) = ct.notify {
                                nick = util::string_to_irc_nick(name);
                            }
                        }
                        info!("Creating new (WA) recipient for {} (nick {})", addr, nick);
                        self.store.store_recipient(&addr, &nick, true)?
                    };
                    self.cf_tx.unbounded_send(ContactFactoryCommand::MakeContact(addr))
                        .unwrap();
                    participants.push(recip.id);
                    if admin {
                        admins.push(recip.id);
                    }
                }
            }
            self.store.store_group(&jid, &chan, participants, admins, &grp.subject)?
        };
        self.on_groups_changed();
        self.cb_respond(format!("Group created (id {}).", grp.id));
        Ok(())
    }
    fn on_groups_changed(&mut self) {
        debug!("Groups changed!");
        self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessGroups)
            .unwrap();
        self.cb_tx.unbounded_send(ControlBotCommand::ProcessGroups)
            .unwrap();
    }
    fn group_remove(&mut self, chan: String) -> Result<()> {
        if let Some(grp) = self.store.get_group_by_chan_opt(&chan)? {
            self.store.delete_group_with_id(grp.id)?;
            self.on_groups_changed();
            self.cb_respond(format!("Group with jid {} deleted.", grp.jid));
        }
        else {
            self.cb_respond(format!("no group with channel {} found!", chan));
        }
        Ok(())
    }
    fn on_persistent_session_data_changed(&mut self, ps: WaPersistentSession) -> Result<()> {
        self.store.store_wa_persistence(ps)?;
        info!("Persistent session data updated");
        Ok(())
    }
    fn on_state_changed(&mut self, state: WaState) {
        self.connected = state == WaState::Connected;
        self.state = state;
        info!("State changed to {:?}", state);
    }
    fn on_disconnect(&mut self, reason: WaDisconnectReason) {
        use self::WaDisconnectReason::*;

        let reason = match reason {
            Replaced => "connection replaced by another",
            Removed => "connection removed from mobile app"
        };
        warn!("Disconnected from WhatsApp - reason: {:?}", reason);
        self.connected = false;
    }
    fn on_user_data_changed(&mut self, ud: WaUserData) {
        trace!("user data changed: {:?}", ud);
        use self::WaUserData::*;
        match ud {
            ContactsInitial(cts) => {
                info!("Received initial contact list");
                for ct in cts {
                    self.contacts.insert(ct.jid.clone(), ct);
                }
            },
            ContactAddChange(ct) => {
                info!("Contact {} added or modified", ct.jid.to_string());
                self.contacts.insert(ct.jid.clone(), ct);
            },
            ContactDelete(jid) => {
                info!("Contact {} deleted", jid.to_string());
                self.contacts.remove(&jid);
            },
            Chats(cts) => {
                info!("Received initial chat list");
                for ct in cts {
                    self.chats.insert(ct.jid.clone(), ct);
                }
            },
            ChatAction(jid, act) => {
                use whatsappweb::ChatAction::*;
                match act {
                    Remove => {
                        info!("Chat {} removed", jid.to_string());
                        self.chats.remove(&jid);
                    },
                    act => info!("Chat {} action: {:?}", jid.to_string(), act)
                }
            },
            UserJid(jid) => {
                info!("Our jid is: {}", jid.to_string());
                self.our_jid = Some(jid);
            },
            PresenceChange(jid, ps, dt) => {
                use whatsappweb::PresenceStatus::*;

                debug!("JID {} changed presence to {:?} (ts {:?})", jid.to_string(), ps, dt);
                if let Some(num) = util::jid_to_address(&jid) {
                    let away = match ps {
                        Unavailable => {
                            if let Some(ts) = dt {
                                Some(format!("last seen {}", ts))
                            }
                            else {
                                Some("currently offline".into())
                            }
                        },
                        _ => None
                    };
                    debug!("Setting presence for {} to {:?}", num, away);
                    self.cf_tx.unbounded_send(ContactFactoryCommand::UpdateAway(num, away))
                        .unwrap();
                }
            },
            MessageAck(ack) => {
                // TODO: make something more of this
                debug!("Message ack: {:?}", ack);
            },
            GroupIntroduce { newly_created, meta, .. } => {
                info!("Got info for group '{}' (jid {})", meta.subject, meta.id.to_string());
                if newly_created {
                    info!("Group {} was newly created.", meta.id.to_string());
                }
                self.groups.insert(meta.id.clone(), meta);
            },
            GroupParticipantsChange { group, change, inducer, participants } => {
                use whatsappweb::GroupParticipantsChange::*;

                warn!("Participants {:?} in group {} changed: {:?} (by {:?})", participants, group.to_string(), change, inducer);
                // FIXME: actually bridge these changes (!)
                if let Some(group) = self.groups.get_mut(&group) {
                    match change {
                        Add => {
                            for p in participants {
                                group.participants.push((p, false));
                            }
                        },
                        Remove => {
                            group.participants.retain(|(p, _)| !participants.contains(&p));
                        },
                        x @ Promote | x @ Demote => {
                            for &mut (ref p, ref mut admin) in group.participants.iter_mut() {
                                if participants.contains(p) {
                                    let change = if let Promote = x {
                                        true
                                    }
                                    else {
                                        false
                                    };
                                    *admin = change;
                                }
                            }
                        },
                    }
                }
            },
            Battery(level) => {
                debug!("Phone battery level: {}", level);
            }
        }
    }
}
