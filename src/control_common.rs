//! Common behaviours for the control bot.

use futures::sync::mpsc::UnboundedSender;
use comm::{WhatsappCommand, ContactFactoryCommand, ModemCommand};
use util::Result;

static HELPTEXT: &str = r#"sms-irc help:
[in this admin room]
- !csq: check modem signal quality
- !reg: check modem registration status
- !sms <num>: start a conversation with a given phone number
[in a /NOTICE to one of the ghosts]
- !nick <nick>: change nickname
- !wasetup: set up WhatsApp Web integration
- !walogon: logon to WhatsApp Web using stored credentials
- !wabridge <jid> <#channel>: bridge the WA group <jid> to an IRC channel <#channel>
- !walist: list available WA groups
- !wadel <#channel>: unbridge IRC channel <#channel>
"#;

pub trait ControlCommon {
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand>;
    fn cf_tx(&mut self) -> &mut UnboundedSender<ContactFactoryCommand>;
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand>;
    fn send_cb_message(&mut self, msg: &str) -> Result<()>;
    fn process_admin_command(&mut self, mesg: String) -> Result<()> {
        if mesg.len() < 1 || mesg.chars().nth(0) != Some('!') {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        match msg[0] {
            "!wasetup" => {
                self.wa_tx().unbounded_send(WhatsappCommand::StartRegistration)
                    .unwrap();
            },
            "!walist" => {
                self.wa_tx().unbounded_send(WhatsappCommand::GroupList)
                    .unwrap();
            },
            "!walogon" => {
                self.wa_tx().unbounded_send(WhatsappCommand::LogonIfSaved)
                    .unwrap();
            },
            "!wadel" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!wadel takes an argument.")?;
                    return Ok(());
                }
                self.wa_tx().unbounded_send(WhatsappCommand::GroupRemove(msg[1].into()))
                    .unwrap();
            },
            "!wabridge" => {
                if msg.get(1).is_none() || msg.get(2).is_none() {
                    self.send_cb_message("!wabridge takes two arguments.")?;
                    return Ok(());
                }
                let jid = match msg[1].parse() {
                    Ok(j) => j,
                    Err(e) => {
                        self.send_cb_message(&format!("failed to parse jid: {}", e))?;
                        return Ok(());
                    }
                };
                self.wa_tx().unbounded_send(WhatsappCommand::GroupAssociate(jid, msg[2].into()))
                    .unwrap();
            },
            "!csq" => {
                self.m_tx().unbounded_send(ModemCommand::RequestCsq).unwrap();
            },
            "!reg" => {
                self.m_tx().unbounded_send(ModemCommand::RequestReg).unwrap();
            },
            "!sms" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!sms takes an argument.")?;
                    return Ok(());
                }
                let addr = msg[1].parse().unwrap();
                self.cf_tx().unbounded_send(ContactFactoryCommand::MakeContact(addr)).unwrap();
            },
            "!help" => {
                for line in HELPTEXT.lines() {
                    self.send_cb_message(line)?;
                }
            },
            unrec => {
                self.send_cb_message(&format!("Unknown command: {}", unrec))?;
            }
        }
        Ok(())
    }
}
