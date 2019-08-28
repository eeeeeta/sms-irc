//! WhatsApp message processing tools.

use whatsappweb::{Jid, MediaType};
use chrono::prelude::*;
use futures::sync::mpsc::UnboundedSender;
use whatsappweb::message::{ChatMessageContent, QuotedChatMessage, MessageId, Peer};
use regex::{Regex, Captures};
use huawei_modem::pdu::PduAddress;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

use crate::comm::WhatsappCommand;
use crate::store::Store;
use crate::whatsapp_media::{MediaInfo, self};
use crate::util::Result;

pub struct IncomingMessage {
    pub id: MessageId,
    pub peer: Option<Peer>, 
    pub from: Jid,
    pub group: Option<i32>,
    pub content: ChatMessageContent,
    pub quoted: Option<QuotedChatMessage>,
    pub ts: NaiveDateTime
}
pub struct ProcessedIncomingMessage {
    pub from: Jid,
    pub text: String,
    pub group: Option<i32>,
    pub ts: NaiveDateTime
}
pub struct WaMessageProcessor {
    pub(crate) store: Store,
    pub(crate) media_path: String,
    pub(crate) dl_path: String,
    pub(crate) wa_tx: Arc<UnboundedSender<WhatsappCommand>>
}

impl WaMessageProcessor {
    fn process_incoming_media(&mut self, id: MessageId, peer: Option<Peer>, from: Jid, group: Option<i32>, ct: ChatMessageContent, ts: NaiveDateTime) -> Result<()> {

        let (ty, fi, name) = match ct {
            ChatMessageContent::Image { info, .. } => (MediaType::Image, info, None),
            ChatMessageContent::Video { info, .. } => (MediaType::Video, info, None),
            ChatMessageContent::Audio { info, .. } => (MediaType::Audio, info, None),
            ChatMessageContent::Document { info, filename } => (MediaType::Document, info, Some(filename)),
            _ => unreachable!()
        };
        let mi = MediaInfo {
            ty, fi, name, peer, ts,
            mi: id,
            from, group,
            path: self.media_path.clone(),
            dl_path: self.dl_path.clone(),
            tx: self.wa_tx.clone()
        };
        mi.start();
        Ok(())
    }
    fn process_wa_text_message<'a>(&mut self, msg: &'a str) -> String {
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
    pub fn process_wa_incoming(&mut self, inc: IncomingMessage) -> Result<(Vec<ProcessedIncomingMessage>, bool)> {
        let IncomingMessage { id, peer, from, group, content, ts, quoted } = inc;
        let mut ret = Vec::with_capacity(2);
        let mut is_media = false;
        let text = match content {
            ChatMessageContent::Text(s) => self.process_wa_text_message(&s),
            ChatMessageContent::Unimplemented(mut det) => {
                if det.trim() == "" {
                    debug!("Discarding empty unimplemented message.");
                    return Ok((ret, is_media));
                }
                if det.len() > 128 {
                    det = det.graphemes(true)
                        .take(128)
                        .chain(std::iter::once("…"))
                        .collect();
                }
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
                match whatsapp_media::store_contact(&self.media_path, &self.dl_path, vcard) {
                    Ok(link) => {
                        format!("\x01ACTION uploaded a contact for '{}' - {}\x01", display_name, link)
                    },
                    Err(e) => {
                        warn!("Failed to save contact card: {}", e);
                        format!("\x01ACTION uploaded a contact for '{}' (couldn't download)\x01", display_name)
                    }
                }
            },
            mut x @ ChatMessageContent::Image { .. } |
                mut x @ ChatMessageContent::Video { .. } |
                mut x @ ChatMessageContent::Audio { .. } |
                mut x @ ChatMessageContent::Document { .. } => {
                    let capt = x.take_caption();
                    self.process_incoming_media(id.clone(), peer.clone(), from.clone(), group, x, ts)?;
                    is_media = true;
                    if let Some(c) = capt {
                        c
                    }
                    else {
                        return Ok((ret, is_media));
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
                message = message.graphemes(true)
                    .take(128)
                    .chain(std::iter::once("…"))
                    .collect();
            }
            let quote = format!("\x0315> \x1d{}{}", nick, message);
            ret.push(ProcessedIncomingMessage {
                from: from.clone(),
                text: quote,
                group,
                ts
            });
        }
        ret.push(ProcessedIncomingMessage {
            from,
            text,
            group,
            ts
        });
        Ok((ret, is_media))
    }
}
