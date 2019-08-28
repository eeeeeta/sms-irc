//! Tracks WhatsApp message acknowledgements, and alerts if we don't get any.

use tokio_timer::Interval;
use whatsappweb::Jid;
use whatsappweb::message::{ChatMessageContent, MessageAckLevel, MessageAck};
use chrono::prelude::*;
use std::collections::HashMap;
use futures::sync::mpsc::UnboundedSender;
use unicode_segmentation::UnicodeSegmentation;
use std::time::{Instant, Duration};
use futures::{Future, Async, Poll, Stream};
use failure::Error;

use crate::comm::{ControlBotCommand, InitParameters};

struct MessageSendStatus {
    ack_level: Option<MessageAckLevel>,
    sent_ts: DateTime<Utc>,
    content: ChatMessageContent,
    destination: Jid,
    alerted: bool,
    alerted_pending: bool,
}
pub struct WaAckTracker {
    cb_tx: UnboundedSender<ControlBotCommand>,
    outgoing_messages: HashMap<String, MessageSendStatus>,
    ack_warn: u64,
    ack_warn_pending: u64,
    ack_expiry: u64,
    timer: Interval,
}
impl Future for WaAckTracker {
    type Item = (); 
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(_) = self.timer.poll()? {
            self.check_acks();
        }
        Ok(Async::NotReady)
    }
}
impl WaAckTracker {
    pub fn new<T>(p: &InitParameters<T>) -> Self {
        let cb_tx = p.cm.cb_tx.clone();
        let ack_ivl = p.cfg.whatsapp.ack_check_interval.unwrap_or(3);
        let ack_warn_ms = p.cfg.whatsapp.ack_warn_ms.unwrap_or(5000);
        let ack_warn_pending_ms = p.cfg.whatsapp.ack_warn_pending_ms.unwrap_or(ack_warn_ms * 2);
        let ack_expiry_ms = p.cfg.whatsapp.ack_expiry_ms.unwrap_or(60000);
        let timer = Interval::new(Instant::now(), Duration::new(ack_ivl, 0));
        Self {
            ack_warn: ack_warn_ms,
            ack_warn_pending: ack_warn_pending_ms,
            ack_expiry: ack_expiry_ms,
            outgoing_messages: HashMap::new(),
            cb_tx, timer
        }
    }
    pub fn register_send(&mut self, to: Jid, content: ChatMessageContent, mid: String) {
        let mss = MessageSendStatus {
            ack_level: None,
            sent_ts: Utc::now(),
            content,
            destination: to,
            alerted: false,
            alerted_pending: false
        };
        self.outgoing_messages.insert(mid, mss);
    }
    pub fn print_acks(&mut self) -> Vec<String> {
        let now = Utc::now();
        let mut lines = vec![];
        for (mid, mss) in self.outgoing_messages.iter_mut() {
            let delta = now - mss.sent_ts;
            let mut summary = mss.content.quoted_description();
            if summary.len() > 15 {
                summary = summary.graphemes(true)
                    .take(10)
                    .chain(std::iter::once("…"))
                    .collect();
            }
            let al: std::borrow::Cow<str> = match mss.ack_level {
                Some(al) => format!("{:?}", al).into(),
                None => "undelivered".into()
            };
            lines.push(format!("- \"\x1d{}\x1d\" to \x02{}\x02 ({}s ago) is \x02{}\x02", 
                               summary, mss.destination.to_string(), delta.num_seconds(), al));
            lines.push(format!("  (message ID \x11{}\x0f)", mid));
        }
        if lines.len() == 0 {
            lines.push("No outgoing messages".into());
        }
        lines
    }
    pub fn on_message_ack(&mut self, ack: MessageAck) {
        if let Some(mss) = self.outgoing_messages.get_mut(&ack.id.0) {
            debug!("Ack known message {} at level: {:?}", ack.id.0, ack.level);
            mss.ack_level = Some(ack.level);
        }
        else {
            debug!("Ack unknown message {} at level: {:?}", ack.id.0, ack.level);
        }
    } 
    fn send_fail<T: Into<String>>(cb_tx: &mut UnboundedSender<ControlBotCommand>, msg: T) {
        cb_tx.unbounded_send(ControlBotCommand::ReportFailure(msg.into()))
            .unwrap();
    }
    fn check_acks(&mut self) {
        trace!("Checking acks");
        let now = Utc::now();
        for (mid, mss) in self.outgoing_messages.iter_mut() {
            let delta = now - mss.sent_ts;
            let delta_ms = delta.num_milliseconds() as u64;
            if mss.ack_level.is_none() {
                if delta_ms >= self.ack_warn && !mss.alerted {
                    warn!("Message {} has been un-acked for {} seconds!", mid, delta.num_seconds());
                    Self::send_fail(&mut self.cb_tx, format!("Warning: Sending message ID {} has failed, or is taking longer than usual!", mid));
                    mss.alerted = true;
                }
            }
            if let Some(MessageAckLevel::PendingSend) = mss.ack_level {
                if delta_ms >= self.ack_warn_pending && !mss.alerted_pending {
                    warn!("Message {} has been pending for {} seconds!", mid, delta.num_seconds());
                    Self::send_fail(&mut self.cb_tx, format!("Warning: Sending message ID {} is still pending. Is WhatsApp running and connected?", mid));
                    mss.alerted_pending = true;
                }
            }
        }
        let ack_expiry = self.ack_expiry;
        self.outgoing_messages.retain(|_, m| {
            if let Some(MessageAckLevel::PendingSend) | None = m.ack_level {
                // Always retain the failures, so the user knows what happened with them (!)
                return true;
            }
            let diff_ms = (now - m.sent_ts).num_milliseconds() as u64;
            diff_ms < ack_expiry
        });
    }
}
