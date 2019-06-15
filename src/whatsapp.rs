//! Experimental support for WhatsApp.

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
use crate::store::Store;
use std::sync::Arc;
use crate::comm::{WhatsappCommand, ContactFactoryCommand, ControlBotCommand, InitParameters};
use crate::util::{self, Result};
use image::Luma;
use qrcode::QrCode;
use futures::{Future, Async, Poll, Stream};
use failure::Error;
use crate::models::Recipient;
use crate::whatsapp_media::{MediaInfo, MediaResult};
use regex::{Regex, Captures};

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
    state: WaState,
    connected: bool,
    store: Store,
    qr_path: String,
    media_path: String,
    dl_path: String,
    autocreate: Option<String>,
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
        let qr_path = p.cfg.whatsapp.qr_path.clone().unwrap_or("/tmp/wa_qr.png".into());
        let media_path = p.cfg.whatsapp.media_path.clone().unwrap_or("/tmp/wa_media".into());
        let dl_path = p.cfg.whatsapp.dl_path.clone().unwrap_or("file:///tmp/wa_media".into());
        let autocreate = p.cfg.whatsapp.autocreate_prefix.clone();
        wa_tx.unbounded_send(WhatsappCommand::LogonIfSaved)
            .unwrap();
        Self {
            conn: None,
            contacts: HashMap::new(),
            chats: HashMap::new(),
            state: WaState::Uninitialized,
            connected: false,
            our_jid: None,
            wa_tx: Arc::new(wa_tx),
            rx, cf_tx, cb_tx, qr_path, store, media_path, dl_path, autocreate
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
            GroupAssociate(jid, to) => self.group_associate_handler(jid, to)?,
            GroupList => self.group_list()?,
            GroupUpdateAll => self.group_update_all()?,
            GroupRemove(grp) => self.group_remove(grp)?,
            StateChanged(was) => self.on_state_changed(was),
            UserDataChanged(wau) => self.on_user_data_changed(wau)?,
            PersistentChanged(wap) => self.on_persistent_session_data_changed(wap)?,
            Disconnect(war) => self.on_disconnect(war),
            GotGroupMetadata(meta) => self.on_got_group_metadata(meta)?,
            Message(new, msg) => {
                if new {
                    self.on_message(msg)?;
                }
            },
            MediaFinished(r) => self.media_finished(r)?,
            AvatarUrl(pdua, url) => self.avatar_url(pdua, url)?,
            AvatarUpdate(nick) => self.avatar_update_by_nick(&nick)?,
            AvatarShow(nick) => self.avatar_show_by_nick(&nick)?,
            AvatarUpdateAll => self.avatar_update_all()?
        }
        Ok(())
    }
    fn avatar_show_by_nick(&mut self, nick: &str) -> Result<()> {
        if let Some(recip) = self.store.get_recipient_by_nick_opt(nick)? {
            if let Some(au) = recip.avatar_url {
                self.cb_respond(format!("{}'s avatar: {}", nick, au));
            }
            else {
                self.cb_respond(format!("{} doesn't have an avatar.", nick));
            }
        }
        else {
            self.cb_respond("Nick not found in database.".into());
        }
        Ok(())
    }
    fn avatar_update_by_nick(&mut self, nick: &str) -> Result<()> {
        if let Some(recip) = self.store.get_recipient_by_nick_opt(nick)? {
            let addr = util::un_normalize_address(&recip.phone_number)
                .ok_or(format_err!("invalid phone number in db"))?;
            self.cb_respond(format!("Updating avatar for {}...", addr));
            self.avatar_update(addr)?;
        }
        else {
            self.cb_respond("Nick not found in database.".into());
        }
        Ok(())
    }
    fn avatar_update_all(&mut self) -> Result<()> {
        for recip in self.store.get_all_recipients()? {
            if recip.whatsapp {
                let addr = util::un_normalize_address(&recip.phone_number)
                    .ok_or(format_err!("invalid phone number in db"))?;
                self.avatar_update(addr)?;
            }
        }
        Ok(())
    }
    fn avatar_update(&mut self, pdua: PduAddress) -> Result<()> {
        match Jid::from_phonenumber(format!("{}", pdua)) {
            Ok(jid) => {
                if let Some(ref mut conn) = self.conn {
                    let tx2 = self.wa_tx.clone();
                    conn.get_profile_picture(&jid, Box::new(move |pic| {
                        tx2.unbounded_send(WhatsappCommand::AvatarUrl(pdua.clone(), pic.map(|x| x.to_string())))
                            .unwrap();
                    }));
                }
            },
            Err(e) => {
                warn!("couldn't make jid from phonenumber {}: {}", pdua, e);
            }
        }
        Ok(())
    }
    fn avatar_url(&mut self, pdua: PduAddress, url: Option<String>) -> Result<()> {
        if url.is_some() {
            info!("Got new avatar for {}", pdua);
        }
        else {
            info!("Removing old avatar for {}", pdua);
        }
        self.store.update_recipient_avatar_url(&pdua, url)?;
        self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessAvatars)
            .unwrap();
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
            info!("WhatsApp is not configured");
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
    fn process_message<'a>(&mut self, msg: &'a str) -> String {
        lazy_static! {
            static ref BOLD_RE: Regex = Regex::new(r#"\*([^\*]+)\*"#).unwrap();
            static ref ITALICS_RE: Regex = Regex::new(r#"_([^_]+)_"#).unwrap();
            static ref MENTIONS_RE: Regex = Regex::new(r#"@(\d+)"#).unwrap();
        }
        let emboldened = BOLD_RE.replace_all(msg, "\x02$1\x02");
        let italicised = ITALICS_RE.replace_all(&emboldened, "\x1D$1\x1D");
        let store = &mut self.store;
        let ret = MENTIONS_RE.replace_all(&italicised, |caps: &Captures| {
            let pdua: PduAddress = caps[0].replace("@", "+").parse().unwrap();
            match store.get_recipient_by_addr_opt(&pdua) {
                Ok(Some(recip)) => recip.nick,
                Ok(None) => format!("<+{}>", &caps[1]),
                Err(e) => {
                    warn!("Error searching for mention recipient: {}", e);
                    format!("@{}", &caps[1])
                }
            }
        });
        ret.to_string()
    }
    fn jid_to_nick(&mut self, jid: &Jid) -> Result<Option<String>> {
        if let Some(num) = jid.phonenumber() {
            if let Ok(pdua) = num.parse() {
                return Ok(self.store.get_recipient_by_addr_opt(&pdua)?
                    .map(|x| x.nick));
            }
        }
        Ok(None)
    }
    fn on_message(&mut self, msg: Box<WaMessage>) -> Result<()> {
        use whatsappweb::message::{Direction, Peer};

        trace!("processing WA message: {:?}", msg);
        let msg = *msg; // otherwise stupid borrowck gets angry, because Box
        let WaMessage { direction, content, id, quoted, .. } = msg;
        debug!("got message from dir {:?}", direction);
        let mut peer = None;
        let mut is_ours = false;
        let (from, group) = match direction {
            Direction::Sending(jid) => {
                let ojid = self.our_jid.clone()
                    .ok_or(format_err!("our_jid empty"))?;
                is_ours = true;
                let group = if jid.is_group {
                    Some(jid)
                }
                else {
                    info!("Received self-message in a 1-to-1 chat, ignoring...");
                    return Ok(());
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
                    if self.autocreate.is_some() {
                        info!("Attempting to autocreate channel for unbridged group {}...", gid.to_string());
                        match self.group_autocreate_from_unbridged(gid) {
                            Ok(id) => Some(id),
                            Err(e) => {
                                warn!("Autocreation failed: {}", e);
                                return Ok(());
                            }
                        }
                    }
                    else {
                        info!("Received message for unbridged group {}, ignoring...", gid.to_string());
                        return Ok(());
                    }
                }
            },
            None => None
        };
        let mut is_media = false;
        let text = match content {
            ChatMessageContent::Text(s) => self.process_message(&s),
            ChatMessageContent::Unimplemented(mut det) => {
                if det.trim() == "" {
                    debug!("Discarding empty unimplemented message.");
                    return Ok(());
                }
                det.truncate(128);
                format!("[\x02\x0304unimplemented\x0f] {}", det)
            },
            ChatMessageContent::LiveLocation { lat, long, speed, .. } => {
                // FIXME: use write!() maybe
                let spd = if let Some(s) = speed {
                    format!("travelling at {:.02} m/s - https://google.com/maps?q={},{}", s, lat, long)
                }
                else {
                    format!("broadcasting live location - https://google.com/maps?q={},{}", lat, long)
                };
                format!("\x01ACTION is {}\x01", spd)
            },
            ChatMessageContent::Location { lat, long, name, .. } => {
                let place = if let Some(n) = name {
                    format!("at '{}'", n)
                }
                else {
                    "somewhere".into()
                };
                format!("\x01ACTION is {} - https://google.com/maps?q={},{}\x01", place, lat, long)
            },
            ChatMessageContent::Redaction { mid } => {
                // TODO: make this more useful
                format!("\x01ACTION redacted message ID \x11{}\x11\x01", mid.0)
            },
            ChatMessageContent::Contact { display_name, vcard } => {
                match crate::whatsapp_media::store_contact(&self.media_path, &self.dl_path, vcard) {
                    Ok(link) => {
                        format!("\x01ACTION uploaded a contact for '{}' - {}", display_name, link)
                    },
                    Err(e) => {
                        warn!("Failed to save contact card: {}", e);
                        format!("\x01ACTION uploaded a contact for '{}' (couldn't download)", display_name)
                    }
                }
            },
            mut x @ ChatMessageContent::Image { .. } |
                mut x @ ChatMessageContent::Video { .. } |
                mut x @ ChatMessageContent::Audio { .. } |
                mut x @ ChatMessageContent::Document { .. } => {
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
        if let Some(qm) = quoted {
            let nick = if group.is_some() {
                let nick = self.jid_to_nick(&qm.participant)?
                    .unwrap_or(qm.participant.to_string());
                format!("<{}> ", nick)
            }
            else {
                String::new()
            };
            let mut message = qm.content.quoted_description();
            if message.len() > 128 {
                message.truncate(128);
                message.push_str("…");
            }
            let quote = format!("\x0315> \x1d{}{}", nick, message);
            self.store_message(&from, &quote, group)?;
        }
        self.store_message(&from, &text, group)?;
        if let Some(p) = peer {
            if let Some(ref mut conn) = self.conn {
                if !is_media && !is_ours {
                    conn.send_message_read(id, p);
                }
            }
        }
        Ok(())
    }
    fn store_message(&mut self, from: &Jid, text: &str, group: Option<i32>) -> Result<()> {
        if let Some(addr) = util::jid_to_address(from) {
            let _ = self.get_wa_recipient(from, &addr)?;
            self.store.store_plain_message(&addr, &text, group)?;
            self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                .unwrap();
        }
        else {
            warn!("couldn't make address for jid {}", from.to_string());
        }
        Ok(())
    }
    fn process_media(&mut self, id: MessageId, peer: Option<Peer>, addr: PduAddress, group: Option<i32>, ct: ChatMessageContent) -> Result<()> {
        use whatsappweb::MediaType;

        let (ty, fi, name) = match ct {
            ChatMessageContent::Image { info, .. } => (MediaType::Image, info, None),
            ChatMessageContent::Video { info, .. } => (MediaType::Video, info, None),
            ChatMessageContent::Audio { info, .. } => (MediaType::Audio, info, None),
            ChatMessageContent::Document { info, filename } => (MediaType::Document, info, Some(filename)),
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
    fn get_wa_recipient(&mut self, jid: &Jid, addr: &PduAddress) -> Result<Recipient> {
        if let Some(recip) = self.store.get_recipient_by_addr_opt(&addr)? {
            Ok(recip)
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
            info!("Creating new WA recipient for {} (nick {})", addr, nick);
            Ok(self.store.store_recipient(&addr, &nick, true)?)
        }
    }
    fn on_got_group_metadata(&mut self, grp: GroupMetadata) -> Result<()> {
        match self.store.get_group_by_jid_opt(&grp.id)? {
            Some(g) => {
                info!("Got metadata for group '{}' (jid {}, id {})", grp.subject, grp.id.to_string(), g.id);
            },
            None => {
                warn!("Got metadata for unbridged group '{}' (jid {})", grp.subject, grp.id.to_string());
            }
        }
        let mut participants = vec![];
        let mut admins = vec![];
        for &(ref jid, admin) in grp.participants.iter() {
            if let Some(addr) = util::jid_to_address(jid) {
                let recip = self.get_wa_recipient(&jid, &addr)?;
                self.cf_tx.unbounded_send(ContactFactoryCommand::MakeContact(addr, true))
                    .unwrap();
                participants.push(recip.id);
                if admin {
                    admins.push(recip.id);
                }
            }
        }
        self.store.update_group(&grp.id, participants, admins, &grp.subject)?;
        self.on_groups_changed();
        Ok(())
    }
    fn group_update_all(&mut self) -> Result<()> {
        info!("Updating metadata for ALL groups");
        for grp in self.store.get_all_groups()? {
            if let Ok(j) = grp.jid.parse() {
                self.request_update_group(j)?;
            }
        }
        Ok(())
    }
    fn request_update_group(&mut self, jid: Jid) -> Result<()> {
        info!("Getting metadata for jid {}", jid.to_string());
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
        Ok(())
    }
    /// Auto-create a group after a GroupIntroduction message.
    ///
    /// This is the nicest way to autocreate a group, because we get all the metadata straight off.
    fn group_autocreate_from_intro(&mut self, meta: GroupMetadata) -> Result<()> {
        let jid = meta.id.to_string();
        info!("Attempting to autocreate channel for new group {}...", jid);
        let subj = meta.subject.clone();
        match self.group_autocreate(meta) {
            Ok((id, chan)) => {
                self.cb_respond(format!("Automatically bridged new group '{}' to channel {} (id {})", subj, chan, id));
            },
            Err(e) => {
                warn!("Autocreation failed for group {}: {}", jid, e);
                self.cb_respond(format!("Failed to autocreate new group {} - check logs for details.", jid));
            }
        }
        Ok(())
    }
    /// Auto-create a group that we've received an unbridged message for.
    ///
    /// Here, we have to hope that we've got data in our initial chat list for this group;
    /// otherwise, we can't know the subject of the group in order to autocreate it.
    fn group_autocreate_from_unbridged(&mut self, jid: Jid) -> Result<i32> {
        let chat = match self.chats.get(&jid) {
            Some(c) => c.clone(),
            None => bail!("chat not in chat list")
        };
        let name = match chat.name {
            Some(n) => n,
            None => bail!("chat unnamed")
        };
        let irc_subject = util::string_to_irc_chan(&name);
        let chan = format!("{}-{}", self.autocreate.as_ref().unwrap(), irc_subject);
        let id = self.group_associate(jid, chan.clone(), true)?;
        Ok(id)
    }
    fn group_autocreate(&mut self, meta: GroupMetadata) -> Result<(i32, String)> {
        let irc_subject = util::string_to_irc_chan(&meta.subject);
        let chan = format!("{}-{}", self.autocreate.as_ref().unwrap(), irc_subject);
        let id = self.group_associate(meta.id.clone(), chan.clone(), false)?;
        self.on_got_group_metadata(meta)?;
        Ok((id, chan))
    }
    fn group_associate(&mut self, jid: Jid, chan: String, request_update: bool) -> Result<i32> {
        if let Some(grp) = self.store.get_group_by_jid_opt(&jid)? {
            bail!("that group already exists (channel {})!", grp.channel);
        }
        if let Some(grp) = self.store.get_group_by_chan_opt(&chan)? {
            bail!("that channel is already used for a group (jid {})!", grp.jid);
        }
        if self.conn.is_none() || !self.connected {
            bail!("we aren't connected to WhatsApp!");
        }
        if !jid.is_group {
            bail!("that jid isn't a group!");
        }
        info!("Bridging WA group {} to channel {}", jid.to_string(), chan);
        let grp = self.store.store_group(&jid, &chan, vec![], vec![], "*** Group setup in progress, please wait... ***")?;
        if request_update {
            self.request_update_group(jid)?;
        }
        self.on_groups_changed();
        Ok(grp.id)
    }
    fn group_associate_handler(&mut self, jid: Jid, chan: String) -> Result<()> {
        match self.group_associate(jid, chan, true) {
            Ok(_) => self.cb_respond("Group creation successful.".into()),
            Err(e) => self.cb_respond(format!("Group creation failed: {}", e))
        }
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
    fn on_user_data_changed(&mut self, ud: WaUserData) -> Result<()> {
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
                    let cmd = ContactFactoryCommand::ForwardCommand(
                        num,
                        crate::comm::ContactManagerCommand::UpdateAway(away)
                        );
                    self.cf_tx.unbounded_send(cmd)
                        .unwrap();
                }
            },
            MessageAck(ack) => {
                // TODO: make something more of this
                debug!("Message ack: {:?}", ack);
            },
            GroupIntroduce { newly_created, meta, .. } => {
                let is_new = if newly_created { " newly created" } else { "" };
                let jid = meta.id.to_string();
                info!("Introduced{} group '{}' (jid {})", is_new, meta.subject, jid);
                if newly_created && self.autocreate.is_some() {
                    self.group_autocreate_from_intro(meta)?;
                }
            },
            GroupSubjectChange { group, subject, subject_owner, .. } => {
                if let Some(id) = self.store.get_group_by_jid_opt(&group)?.map(|x| x.id) {
                    self.store_message(&subject_owner, &format!("\x01ACTION changed the subject to '{}'\x01", subject), Some(id))?;
                    self.request_update_group(group)?;
                }
            },
            GroupParticipantsChange { group, change, inducer, participants } => {
                use whatsappweb::GroupParticipantsChange;

                debug!("Participants {:?} in group {} changed: {:?} (by {:?})", participants, group.to_string(), change, inducer);
                if let Some(id) = self.store.get_group_by_jid_opt(&group)?.map(|x| x.id) {
                    if let Some(inducer) = inducer {
                        let mut nicks = vec![];
                        for participant in participants {
                            if let Some(addr) = util::jid_to_address(&participant) {
                                let recip = self.get_wa_recipient(&participant, &addr)?;
                                nicks.push(recip.nick);
                            }
                        }
                        let action = match change {
                            GroupParticipantsChange::Add => "added",
                            GroupParticipantsChange::Remove => "removed",
                            GroupParticipantsChange::Promote => "promoted",
                            GroupParticipantsChange::Demote => "demoted",
                        };
                        self.store_message(&inducer, &format!("\x01ACTION {} user(s): {}\x01", action, nicks.join(", ")), Some(id))?;
                    }
                    self.request_update_group(group)?;
                }
            },
            StatusChange(user, status) => {
                if let Some(addr) = util::jid_to_address(&user) {
                    let recip = self.get_wa_recipient(&user, &addr)?;
                    info!("{} changed their status to: {}", recip.nick, status);
                }
            },
            PictureChange { jid, removed } => {
                if let Some(addr) = util::jid_to_address(&jid) {
                    let recip = self.get_wa_recipient(&jid, &addr)?;
                    if !removed {
                        info!("{} changed their profile photo.", recip.nick);
                    }
                    else {
                        info!("{} removed their profile photo.", recip.nick);
                    }
                    self.avatar_update(addr)?;
                }
            },
            Battery(level) => {
                debug!("Phone battery level: {}", level);
            }
        }
        Ok(())
    }
}
