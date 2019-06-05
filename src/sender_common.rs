//! Common behaviours for senders (things which decode PDUs and send the plaintext over IRC).
//!
//! FIXME: the 'from_nick' parameter is a bit hacky.

use crate::models::Message;
use huawei_modem::pdu::DeliverPdu;
use crate::store::Store;
use crate::util::Result;
use std::convert::TryFrom;

/// The maximum message size sent over IRC.
static MESSAGE_MAX_LEN: usize = 350;

pub trait Sender {
    fn report_error(&mut self, _from_nick: &str, _err: String) -> Result<()>;
    fn store(&mut self) -> &mut Store;
    fn private_target(&mut self) -> String;
    fn send_irc_message(&mut self, _from_nick: &str, _to: &str, _msg: &str) -> Result<()>;
    /// Ensure that the admin user is joined to the given channel, if possible.
    ///
    /// This uses, e.g. SVSJOIN to force-join the user to the channel.
    fn ensure_joined(&mut self, _ch: &str) -> Result<()> {
        Ok(())
    }
    fn send_raw_message(&mut self, from_nick: &str, msg: &str, group_target: Option<i32>) -> Result<()> {
        let dest = if let Some(g) = group_target {
            let grp = self.store().get_group_by_id(g)?;
            self.ensure_joined(&grp.channel)?;
            grp.channel
        }
        else {
            self.private_target()
        };
        // We need to split messages that are too long to send on IRC up
        // into fragments, as well as splitting them at newlines.
        //
        // Shoutout to sebk from #rust on moznet for providing
        // this nifty implementation!
        for line in msg.lines() {
            let mut last = 0;
            let iter = line.char_indices().filter_map(|(i, _)| {
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
                self.send_irc_message(from_nick, &dest, chunk)?;
            }
        }
        Ok(())
    }
    fn process_msg_plain(&mut self, nick: &str, msg: Message) -> Result<()> {
        let text = msg.text.as_ref().expect("msg has neither text nor pdu");
        self.send_raw_message(nick, text, msg.group_target)?;
        self.store().delete_message(msg.id)?;
        Ok(())
    }
    fn process_msg_pdu(&mut self, nick: &str, msg: Message, pdu: DeliverPdu) -> Result<()> {
        match pdu.get_message_data().decode_message() {
            Ok(m) => {
                if let Some(cd) = m.udh.and_then(|x| x.get_concatenated_sms_data()) {
                    debug!("Message is concatenated: {:?}", cd);
                    let msgs = self.store().get_all_concatenated(&msg.phone_number, cd.reference as _)?;
                    if msgs.len() != (cd.parts as usize) {
                        debug!("Not enough messages: have {}, need {}", msgs.len(), cd.parts);
                        return Ok(());
                    }
                    let mut concatenated = String::new();
                    let mut pdus = vec![];
                    for msg in msgs.iter() {
                        let dec = DeliverPdu::try_from(msg.pdu.as_ref().expect("csms message has no pdu") as &[u8])?
                            .get_message_data()
                            .decode_message()?;
                        pdus.push(dec);
                    }
                    pdus.sort_by_key(|p| p.udh.as_ref().unwrap().get_concatenated_sms_data().unwrap().sequence);
                    for pdu in pdus {
                        concatenated.push_str(&pdu.text);
                    }
                    self.send_raw_message(nick, &concatenated, msg.group_target)?;
                    for msg in msgs.iter() {
                        self.store().delete_message(msg.id)?;
                    }
                }
                else {
                    self.send_raw_message(nick, &m.text, msg.group_target)?;
                    self.store().delete_message(msg.id)?;
                }
            },
            Err(e) => {
                self.report_error(nick, format!("indecipherable message: {}", e))?;
            }
        }
        Ok(())
    }
}
