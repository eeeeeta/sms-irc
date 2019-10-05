//! Experimental support for WhatsApp.

use whatsappweb::Jid;
use whatsappweb::Contact as WaContact;
use whatsappweb::Chat as WaChat;
use whatsappweb::GroupMetadata;
use whatsappweb::message::ChatMessage as WaMessage;
use whatsappweb::message::{ChatMessageContent, Peer, MessageId};
use whatsappweb::session::PersistentSession as WaPersistentSession;
use whatsappweb::event::WaEvent;
use whatsappweb::req::WaRequest;
use whatsappweb::errors::WaError;
use whatsappweb::errors::DisconnectReason as WaDisconnectReason;
use huawei_modem::pdu::PduAddress;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use std::collections::HashMap;
use std::sync::Arc;
use image::Luma;
use qrcode::QrCode;
use futures::{Future, Async, Poll, Stream, Sink};
use failure::Error;
use chrono::prelude::*;
use std::time::{Instant, Duration};
use std::collections::VecDeque;

use crate::comm::{WhatsappCommand, ContactFactoryCommand, ControlBotCommand, InitParameters};
use crate::util::{self, Result};
use crate::models::Recipient;
use crate::whatsapp_media::MediaResult;
use crate::store::Store;
use crate::whatsapp_conn::{WebConnectionWrapper, WebConnectionWrapperConfig};
use crate::whatsapp_msg::{IncomingMessage, WaMessageProcessor};
use crate::whatsapp_ack::WaAckTracker;

pub struct WhatsappManager {
    conn: WebConnectionWrapper,
    rx: UnboundedReceiver<WhatsappCommand>,
    cf_tx: UnboundedSender<ContactFactoryCommand>,
    cb_tx: UnboundedSender<ControlBotCommand>,
    contacts: HashMap<Jid, WaContact>,
    chats: HashMap<Jid, WaChat>,
    presence_requests: HashMap<Jid, Instant>,
    msgproc: WaMessageProcessor,
    ackp: WaAckTracker,
    backlog_start: Option<chrono::NaiveDateTime>,
    connected: bool,
    store: Store,
    qr_path: String,
    autocreate: Option<String>,
    autoupdate_nicks: bool,
    mark_read: bool,
    track_presence: bool,
    our_jid: Option<Jid>,
    prev_jid: Option<Jid>,
    outbox: VecDeque<WaRequest>
}
impl Future for WhatsappManager {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        // We use this `cont` variable to handle the case where
        // we change the state of `self.conn` during `handle_int_rx()`,
        // and therefore need to poll it again.
        let mut cont = true;
        while cont {
            cont = false;
            while let Async::Ready(evt) = self.conn.poll()? {
                match evt.expect("None from wrapper impossible") {
                    Ok(e) => self.on_wa_event(e)?,
                    Err(e) => self.on_wa_error(e)
                }
            }
            while let Async::Ready(com) = self.rx.poll().unwrap() {
                let com = com.ok_or(format_err!("whatsappmanager rx died"))?;
                self.handle_int_rx(com)?;
                cont = true;
            }
            if self.outbox.len() > 0 {
                if self.conn.is_connected() {
                    while let Some(req) = self.outbox.pop_front() {
                        if let Err(e) = self.conn.start_send(req) {
                            self.on_wa_error(e);
                            break;
                        }
                    }
                }
                else {
                    warn!("Disconnected, so discarding messages in outbox");
                    self.outbox.clear();
                }
            }
            if self.conn.is_connected() {
                self.conn.poll_complete()?;
            }
        }
        self.ackp.poll()?;
        Ok(Async::NotReady)
    }
}
impl WhatsappManager {
    pub fn new<T>(p: InitParameters<T>) -> Self {
        let ackp = WaAckTracker::new(&p);
        let store = p.store.clone();
        let wa_tx = p.cm.wa_tx.clone();
        let rx = p.cm.wa_rx.take().unwrap();
        let cf_tx = p.cm.cf_tx.clone();
        let cb_tx = p.cm.cb_tx.clone();
        let media_path = p.cfg.whatsapp.media_path.clone().unwrap_or("/tmp/wa_media".into());
        let qr_path = format!("{}/qr.png", media_path);
        let dl_path = p.cfg.whatsapp.dl_path.clone().unwrap_or("file:///tmp/wa_media".into());
        let autocreate = p.cfg.whatsapp.autocreate_prefix.clone();
        let backlog_start = p.cfg.whatsapp.backlog_start.clone();
        let mark_read = p.cfg.whatsapp.mark_read;
        let autoupdate_nicks = p.cfg.whatsapp.autoupdate_nicks;
        let backoff_time_ms = p.cfg.whatsapp.backoff_time_ms.unwrap_or(10000);
        let track_presence = p.cfg.whatsapp.track_presence;

        wa_tx.unbounded_send(WhatsappCommand::LogonIfSaved)
            .unwrap();

        let wa_tx = Arc::new(wa_tx);
        let msgproc = WaMessageProcessor { store: store.clone(), media_path, dl_path, wa_tx };

        let conn = WebConnectionWrapper::new(WebConnectionWrapperConfig {
            backoff_time_ms
        });

        Self {
            conn,
            contacts: HashMap::new(),
            chats: HashMap::new(),
            connected: false,
            our_jid: None,
            prev_jid: None,
            presence_requests: HashMap::new(),
            outbox: VecDeque::new(),
            backlog_start,
            rx, cf_tx, cb_tx, qr_path, store, msgproc, autocreate,
            mark_read, autoupdate_nicks, track_presence, ackp
        }
    }
    fn handle_int_rx(&mut self, c: WhatsappCommand) -> Result<()> {
        use self::WhatsappCommand::*;

        match c {
            StartRegistration => self.start_registration()?,
            LogonIfSaved => self.logon_if_saved()?,
            SendGroupMessage(to, cont) => self.send_group_message(to, cont)?,
            SendDirectMessage(to, cont) => self.send_direct_message(to, cont)?,
            GroupAssociate(jid, to) => self.group_associate_handler(jid, to)?,
            GroupList => self.group_list()?,
            GroupUpdateAll => self.group_update_all()?,
            GroupRemove(grp) => self.group_remove(grp)?,
            MediaFinished(r) => self.media_finished(r)?,
            PrintAcks => self.print_acks()?,
            MakeContact(a) => self.make_contact(a)?,
            SubscribePresence(a) => self.subscribe_presence(a)?
        }
        Ok(())
    }
    fn subscribe_presence(&mut self, addr: PduAddress) -> Result<()> {
        match util::address_to_jid(&addr) {
            Ok(from) => {
                let recip = self.get_wa_recipient(&from)?;
                if self.connected {
                    self.cb_respond(format!("Subscribing to presence updates from '{}' (jid {})`", recip.nick, from));
                    self.outbox.push_back(WaRequest::SubscribePresence(from));
                }
                else {
                    self.cb_respond("Error subscribing: not connected to WA");
                }
            },
            Err(_) => {
                self.cb_respond("Error subscribing: invalid PduAddress");
            }
        }
        Ok(())
    }
    fn make_contact(&mut self, addr: PduAddress) -> Result<()> {
        match util::address_to_jid(&addr) {
            Ok(from) => {
                let _ = self.get_wa_recipient(&from)?;
                self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                    .unwrap();
            },
            Err(e) => {
                error!("Couldn't make contact for {}: {}", addr, e);
            }
        }
        Ok(())
    }
    fn print_acks(&mut self) -> Result<()> {
        for line in self.ackp.print_acks() {
            self.cb_respond(line);
        }
        Ok(())
    } 
    fn media_finished(&mut self, r: MediaResult) -> Result<()> {
        match r.result {
            Ok(ret) => {
                debug!("Media download/decryption job for {} / mid {:?} complete.", r.from.to_string(), r.mi);
                self.store_message(&r.from, &ret, r.group, r.ts)?;
            },
            Err(e) => {
                // FIXME: We could possibly retry the download somehow.
                warn!("Decryption job failed for {} / mid {:?}: {}", r.from.to_string(), r.mi, e);
                let msg = "\x01ACTION uploaded media (couldn't download)\x01";
                self.store_message(&r.from, msg, r.group, r.ts)?;
            }
        }
        self.store.store_wa_msgid(r.mi.0.clone())?;
        if self.mark_read {
            if let Some(p) = r.peer {
                self.outbox.push_back(WaRequest::MessageRead {
                    mid: r.mi,
                    peer: p
                });
            }
        }
        Ok(())
    }
    fn logon_if_saved(&mut self) -> Result<()> {
        if let Some(wap) = self.store.get_wa_persistence_opt()? {
            info!("Logging on to WhatsApp Web using stored persistence data");
            self.conn.connect_persistent(wap);
        }
        else {
            info!("WhatsApp is not configured");
        }
        Ok(())
    }
    fn start_registration(&mut self) -> Result<()> {
        info!("Creating a new WhatsApp Web session");
        self.conn.connect_new();
        Ok(())
    }
    fn cb_respond<T: Into<String>>(&mut self, s: T) {
        self.cb_tx.unbounded_send(ControlBotCommand::CommandResponse(s.into()))
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
        self.cb_respond("NB: The code is only valid for a few seconds, so scan quickly!");
        Ok(())
    }
    fn queue_message(&mut self, content: ChatMessageContent, jid: Jid) {
        let mid = MessageId::generate();
        debug!("Queued send to {}: message ID {}", jid, mid.0);
        self.ackp.register_send(jid, content, mid.0, true);
        if self.conn.is_disabled() {
            let err = "Warning: WhatsApp Web is currently not set up, but you've tried to send something.";
            self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err.into())).unwrap();
        }
    }
    fn send_message(&mut self, content: ChatMessageContent, jid: Jid) -> Result<()> {
        let (c, j) = (content.clone(), jid.clone());
        let m = WaMessage::new(jid, content);
        debug!("Send to {}: message ID {}", j, m.id.0);
        self.store.store_wa_msgid(m.id.0.clone())?;
        self.ackp.register_send(j.clone(), c, m.id.0.clone(), false);
        self.outbox.push_back(WaRequest::SendMessage(m));
        if !j.is_group && self.track_presence {
            let mut update = true;
            let now = Instant::now();
            if let Some(inst) = self.presence_requests.get(&j) {
                // WhatsApp stops sending you presence updates after about 10 minutes.
                // To avoid this, we resubscribe about every 5.
                if now.duration_since(*inst) < Duration::new(300, 0) {
                    update = false;
                }
            }
            if update {
                debug!("Requesting presence updates for {}", j);
                self.presence_requests.insert(j.clone(), now);
                self.outbox.push_back(WaRequest::SubscribePresence(j));
            }
        }
        Ok(())
    }
    fn send_direct_message(&mut self, addr: PduAddress, content: String) -> Result<()> {
        debug!("Sending direct message to {}...", addr);
        trace!("Message contents: {}", content);
        match Jid::from_phonenumber(format!("{}", addr)) {
            Ok(jid) => {
                let content = ChatMessageContent::Text(content);
                if !self.connected || !self.conn.is_connected() {
                    self.queue_message(content, jid);
                }
                else {
                    self.send_message(content, jid)?;
                }
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
        if let Some(grp) = self.store.get_group_by_chan_opt(&chan)? {
            let jid = grp.jid.parse().expect("bad jid in DB");
            let content = ChatMessageContent::Text(content);
            if !self.connected || !self.conn.is_connected() {
                self.queue_message(content, jid);
            }
            else {
                self.send_message(content, jid)?;
            }
        }
        else {
            error!("Tried to send WA message to nonexistent group {}", chan);
        }
        Ok(())
    } 
    fn on_message(&mut self, msg: WaMessage, is_new: bool) -> Result<()> {
        use whatsappweb::message::{Direction};

        trace!("processing WA message (new {}): {:?}", is_new, msg);
        let WaMessage { direction, content, id, quoted, .. } = msg;
        debug!("got message from dir {:?}", direction);
        // If we don't mark things as read, we have to check every 'new' message,
        // because they might not actually be new.
        if !self.mark_read || !is_new {
            if self.store.is_wa_msgid_stored(&id.0)? {
                debug!("Rejecting backlog message: already in database");
                return Ok(());
            }
        }
        if !is_new {
            debug!("message timestamp: {}", msg.time);
            if let Some(ref bsf) = self.backlog_start {
                if *bsf > msg.time {
                    debug!("Rejecting backlog message: before backlog start time");
                    return Ok(());
                }
            }
        }
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
                    debug!("Received self-message in a 1-to-1 chat, ignoring...");
                    self.store.store_wa_msgid(id.0.clone())?;
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
                if gid.id == "status" {
                    return Ok(());
                }
                if let Some(grp) = self.store.get_group_by_jid_opt(&gid)? {
                    Some(grp.id)
                }
                else {
                    if self.autocreate.is_some() {
                        info!("Attempting to autocreate channel for unbridged group {}...", gid);
                        match self.group_autocreate_from_unbridged(gid.clone()) {
                            Ok(id) => Some(id),
                            Err(e) => {
                                warn!("Autocreation failed: {} - requesting metadata instead", e);
                                self.request_update_group(gid)?;
                                return Ok(());
                            }
                        }
                    }
                    else {
                        info!("Received message for unbridged group {}, ignoring...", gid);
                        return Ok(());
                    }
                }
            },
            None => None
        };
        let inc = IncomingMessage { 
            id: id.clone(),
            peer: peer.clone(),
            ts: msg.time,
            from, group, content, quoted
        };
        let (msgs, is_media) = self.msgproc.process_wa_incoming(inc)?;
        for msg in msgs {
            self.store_message(&msg.from, &msg.text, msg.group, msg.ts)?;
        }
        if !is_media {
            self.store.store_wa_msgid(id.0.clone())?;
        }
        if let Some(p) = peer {
            if !is_media && !is_ours && self.mark_read {
                self.outbox.push_back(WaRequest::MessageRead {
                    mid: id,
                    peer: p
                });
            }
        }
        Ok(())
    }
    fn store_message(&mut self, from: &Jid, text: &str, group: Option<i32>, ts: NaiveDateTime) -> Result<()> {
        if let Some(addr) = util::jid_to_address(from) {
            let _ = self.get_wa_recipient(from)?;
            self.store.store_wa_message(&addr, &text, group, ts)?;
            self.cf_tx.unbounded_send(ContactFactoryCommand::ProcessMessages)
                .unwrap();
        }
        else {
            warn!("couldn't make address for jid {}", from.to_string());
        }
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
            list.push(format!("- '{}' (jid {}) - {}", gmeta.name.as_ref().map(|x| x as &str).unwrap_or("<unnamed>"), jid, bstatus));
        }
        if list.len() == 0 {
            self.cb_respond("no WhatsApp chats (yet?)");
        }
        else {
            self.cb_respond("WhatsApp chats:");
        }
        for item in list {
            self.cb_respond(item);
        }
        Ok(())
    }
    fn get_nick_for_jid(&mut self, jid: &Jid) -> Result<(String, i32)> {
        if let Some(ct) = self.contacts.get(jid) {
            if let Some(ref name) = ct.name {
                let nick = util::string_to_irc_nick(&name);
                return Ok((nick, Recipient::NICKSRC_WA_CONTACT));
            }
            else if let Some(ref name) = ct.notify {
                let nick = util::string_to_irc_nick(&name);
                return Ok((nick, Recipient::NICKSRC_WA_NOTIFY));
            }
        }
        let addr = match util::jid_to_address(jid) {
            Some(a) => a,
            None => {
                return Err(format_err!("couldn't translate jid {} to address", jid));
            }
        };
        Ok((util::make_nick_for_address(&addr), Recipient::NICKSRC_AUTO))
    }
    fn get_wa_recipient(&mut self, jid: &Jid) -> Result<Recipient> {
        let addr = match util::jid_to_address(jid) {
            Some(a) => a,
            None => {
                return Err(format_err!("couldn't translate jid {} to address", jid));
            }
        };
        if let Some(recip) = self.store.get_recipient_by_addr_opt(&addr)? {
            Ok(recip)
        }
        else {
            let (nick, nicksrc) = self.get_nick_for_jid(jid)?;
            info!("Creating new WA recipient for {} (nick {}, src {})", addr, nick, nicksrc);
            let notify = self.contacts.get(jid).and_then(|x| x.notify.as_ref().map(|x| x as &str));
            let ret = self.store.store_wa_recipient(&addr, &nick, notify, nicksrc)?;
            self.cf_tx.unbounded_send(ContactFactoryCommand::SetupContact(addr.clone()))
                .unwrap();
            Ok(ret)
        }
    }
    fn on_got_group_metadata(&mut self, grp: GroupMetadata) -> Result<()> {
        match self.store.get_group_by_jid_opt(&grp.id)? {
            Some(g) => {
                info!("Got metadata for group '{}' (jid {}, id {})", grp.subject, grp.id, g.id);
            },
            None => {
                warn!("Got metadata for unbridged group '{}' (jid {})", grp.subject, grp.id);
                if self.autocreate.is_some() {
                    match self.group_autocreate(grp.clone()) {
                        Ok((id, chan)) => {
                            self.cb_respond(format!("Automatically bridged new group '{}' to channel {} (id {})", grp.subject, chan, id));
                        },
                        Err(e) => {
                            warn!("Autocreation failed for group {}: {}", grp.id, e);
                            self.cb_respond(format!("Failed to autocreate new group {} - check logs for details.", grp.id));
                        }
                    }
                }
            }
        }
        let mut participants = vec![];
        let mut admins = vec![];
        for &(ref jid, admin) in grp.participants.iter() {
            let recip = self.get_wa_recipient(jid)?;
            participants.push(recip.id);
            if admin {
                admins.push(recip.id);
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
        info!("Getting metadata for jid {}", jid);
        self.outbox.push_back(WaRequest::GetGroupMetadata(jid));
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
        if !self.connected || !self.conn.is_connected() {
            bail!("we aren't connected to WhatsApp!");
        }
        if !jid.is_group {
            bail!("that jid isn't a group!");
        }
        info!("Bridging WA group {} to channel {}", jid, chan);
        let grp = self.store.store_group(&jid, &chan, vec![], vec![], "*** Group setup in progress, please wait... ***")?;
        if request_update {
            self.request_update_group(jid)?;
        }
        self.on_groups_changed();
        Ok(grp.id)
    }
    fn group_associate_handler(&mut self, jid: Jid, chan: String) -> Result<()> {
        match self.group_associate(jid, chan, true) {
            Ok(_) => self.cb_respond("Group creation successful."),
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
    fn on_established(&mut self, jid: Jid, ps: WaPersistentSession) -> Result<()> {
        self.our_jid = Some(jid.clone());
        if self.our_jid != self.prev_jid {
            info!("Logged in as {}.", jid);
            if self.prev_jid.is_some() {
                warn!("Logged in as a different phone number (prev was {})!", self.prev_jid.as_ref().unwrap());
                let err = "Warning: You've logged in with a different phone number.";
                self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err.into())).unwrap();
            }
        }
        else {
            debug!("Logged in again after connection loss.");
        }
        let unsent = self.ackp.extract_unsent();
        if unsent.len() > 0 {
            info!("Sending {} messages sent while offline", unsent.len());
        }
        for mss in unsent {
            self.send_message(mss.content, mss.destination)?;
        }
        self.store.store_wa_persistence(ps.clone())?;
        self.conn.set_persistent(Some(ps));
        self.prev_jid = Some(jid);
        self.connected = true;
        Ok(())
    }
    fn on_wa_error(&mut self, err: WaError) {
        debug!("WA connection failed: {}", err);
        if let WaError::Disconnected(reason) = err {
            use self::WaDisconnectReason::*;
            let reason_text = match reason {
                Replaced => "connection replaced by another",
                Removed => "connection removed from mobile app"
            };
            warn!("Disconnected from WhatsApp - reason: {:?}", reason_text);
            if let Removed = reason {
                let err = "Error: WhatsApp Web connection removed in the mobile app! Use the WHATSAPP SETUP command to restore connectivity.";
                self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err.into()))
                    .unwrap();
                self.conn.disable();
            }
        }
        if let WaError::StatusCode(sc) = err {
            if sc == 401 { 
                warn!("Disconnected from WhatsApp due to 401");
                let err = "Error: WhatsApp Web credentials are invalid. Use the WHATSAPP SETUP command to restore connectivity.";
                self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err.into()))
                    .unwrap();
                self.conn.disable();
            }
        }
        self.our_jid = None;
        self.connected = false;
    }
    fn on_contact_change(&mut self, ct: WaContact) -> Result<()> {
        let jid = ct.jid.clone();
        if jid.id == "status" || jid.is_group {
            return Ok(());
        }
        if let Some(addr) = util::jid_to_address(&jid) {
            self.contacts.insert(ct.jid.clone(), ct);
            let recip = self.get_wa_recipient(&jid)?;
            let (new_nick, newsrc) = self.get_nick_for_jid(&jid)?;
            let notify = self.contacts.get(&jid).and_then(|x| x.notify.as_ref().map(|x| x as &str));
            if notify.is_some() {
                self.store.update_recipient_notify(&addr, notify)?;
            }
            if recip.nick != new_nick {
                debug!("New nick '{}' (src {}) for recipient {} (from '{}', src {})", new_nick, newsrc, addr, recip.nick, recip.nicksrc);
                let should_update = match (recip.nicksrc, newsrc) {
                    // Migrated nicks should always be changed.
                    (Recipient::NICKSRC_MIGRATED, _) => true,
                    // Don't replace ugly phone numbers with more ugly phone numbers.
                    (Recipient::NICKSRC_AUTO, Recipient::NICKSRC_AUTO) => false,
                    // But replace ugly phone numbers with anything else!
                    (Recipient::NICKSRC_AUTO, _) => true,
                    // Replace WA notify (i.e. name the contact gave themselves) with WA contact
                    // (i.e. name the user gave the contact on their phone's address book)
                    (Recipient::NICKSRC_WA_NOTIFY, Recipient::NICKSRC_WA_CONTACT) => true,
                    // Allow users to update their address book names.
                    (Recipient::NICKSRC_WA_CONTACT, Recipient::NICKSRC_WA_CONTACT) => true,
                    // Update WA notify values as well.
                    (Recipient::NICKSRC_WA_NOTIFY, Recipient::NICKSRC_WA_NOTIFY) => true,
                    // Other changes are probably unwanted.
                    _ => false
                };
                if should_update && self.autoupdate_nicks {
                    info!("Automatically updating nick for {} to {} (oldsrc {}, newsrc {})", addr, new_nick, recip.nicksrc, newsrc);
                    let cmd = ContactFactoryCommand::ForwardCommand(
                        addr,
                        crate::comm::ContactManagerCommand::ChangeNick(new_nick, newsrc)
                        );
                    self.cf_tx.unbounded_send(cmd)
                        .unwrap();
                }
            }
        }
        Ok(())
    }
    fn on_wa_event(&mut self, evt: WaEvent) -> Result<()> {
        use self::WaEvent::*;
        match evt {
            WebsocketConnected => {},
            ScanCode(qr) => self.on_qr(qr)?,
            SessionEstablished { jid, persistent } => self.on_established(jid, persistent)?,
            Message { is_new, msg } => self.on_message(msg, is_new)?,
            InitialContacts(cts) => {
                debug!("Received initial contact list");
                for ct in cts {
                    self.on_contact_change(ct)?;
                }
            },
            AddContact(ct) => {
                debug!("Contact {} added or modified", ct.jid);
                self.on_contact_change(ct)?;
            },
            DeleteContact(jid) => {
                // NOTE: I don't see any real reason to ever delete contacts.
                // They might contain useful names for us to use later, and WA
                // seems to send contact deletion messages on the regular as
                // people send stuff, which is stupid.
                //
                // Therefore, we just don't.
                debug!("Contact {} deleted", jid);
                //self.contacts.remove(&jid);
            },
            InitialChats(cts) => {
                debug!("Received initial chat list");
                for ct in cts {
                    self.chats.insert(ct.jid.clone(), ct);
                }
            },
            ChatEvent { jid, event } => {
                use whatsappweb::ChatAction::*;
                match event {
                    Remove => {
                        debug!("Chat {} removed", jid);
                        self.chats.remove(&jid);
                    },
                    act => debug!("Chat {} action: {:?}", jid, act)
                }
            },
            PresenceChange { jid, presence, ts } => {
                use whatsappweb::PresenceStatus::*;

                debug!("JID {} changed presence to {:?} (ts {:?})", jid, presence, ts);
                if let Some(num) = util::jid_to_address(&jid) {
                    let away = match presence {
                        Unavailable => {
                            if let Some(ts) = ts {
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
                self.ackp.on_message_ack(ack);
            },
            GroupIntroduce { newly_created, meta, .. } => {
                let is_new = if newly_created { " newly created" } else { "" };
                let jid = meta.id.to_string();
                info!("Introduced{} group '{}' (jid {})", is_new, meta.subject, jid);
                if newly_created && self.autocreate.is_some() {
                    self.group_autocreate_from_intro(meta)?;
                }
            },
            GroupMetadata { meta } => {
                match meta {
                    Ok(m) => self.on_got_group_metadata(m)?,
                    Err(e) => {
                        warn!("Group metadata query failed: {}", e);
                    }
                }
            },
            GroupSubjectChange { jid, subject, inducer, .. } => {
                if let Some(id) = self.store.get_group_by_jid_opt(&jid)?.map(|x| x.id) {
                    let ts = Utc::now().naive_utc();
                    self.store_message(&inducer, &format!("\x01ACTION changed the subject to '{}'\x01", subject), Some(id), ts)?;
                    self.request_update_group(jid)?;
                }
            },
            GroupParticipantsChange { jid, change, inducer, participants } => {
                use whatsappweb::GroupParticipantsChange;

                debug!("Participants {:?} in group {} changed: {:?} (by {:?})", participants, jid, change, inducer);
                let ts = Utc::now().naive_utc();
                if let Some(id) = self.store.get_group_by_jid_opt(&jid)?.map(|x| x.id) {
                    if let Some(inducer) = inducer {
                        // Special-case: someone removing themself is just them leaving.
                        if participants.len() == 1 && participants[0] == inducer {
                            self.store_message(&inducer, "\x01ACTION left the group\x01", Some(id), ts)?;
                        }
                        else {
                            let mut nicks = vec![];
                            for participant in participants {
                                let recip = self.get_wa_recipient(&participant)?;
                                nicks.push(recip.nick);
                            }
                            let action = match change {
                                GroupParticipantsChange::Add => "added",
                                GroupParticipantsChange::Remove => "removed",
                                GroupParticipantsChange::Promote => "promoted",
                                GroupParticipantsChange::Demote => "demoted",
                            };
                            self.store_message(&inducer, &format!("\x01ACTION {}: {}\x01", action, nicks.join(", ")), Some(id), ts)?;
                        }
                    }
                    self.request_update_group(jid)?;
                }
            },
            ProfileStatus { jid, status, was_request } => {
                if jid.is_group {
                    warn!("Got profile status for non-user jid {}", jid);
                    return Ok(());
                }
                let recip = self.get_wa_recipient(&jid)?;
                if !was_request {
                    info!("{} changed their status to: {}", recip.nick, status);
                }
            },
            PictureChange { jid, removed } => {
                if jid.is_group {
                    warn!("Got picture change for non-user jid {}", jid);
                    return Ok(());
                }
                let recip = self.get_wa_recipient(&jid)?;
                if !removed {
                    info!("{} changed their profile photo.", recip.nick);
                }
                else {
                    info!("{} removed their profile photo.", recip.nick);
                }
            },
            MessageSendFail { mid, status } => {
                error!("Got a MessageSendFail (status {}) for mid {}", status, mid.0);
                let err = format!("Error: Sending WhatsApp message ID {} failed with code {}!", mid.0, status);
                self.cb_tx.unbounded_send(ControlBotCommand::ReportFailure(err.into()))
                    .unwrap();
            },
            BatteryLevel(level) => {
                // FIXME: warn when this gets low?
                debug!("Phone battery level: {}", level);
            },
            _ => {}
        }
        Ok(())
    }
}
