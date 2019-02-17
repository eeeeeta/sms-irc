//! Common behaviours for the control bot.

use futures::sync::mpsc::UnboundedSender;
use comm::{WhatsappCommand, ContactFactoryCommand, ModemCommand};
use util::Result;

static HELPTEXT: &str = r#"sms-irc help:
[in a /NOTICE to one of the ghosts]
- !nick <nick>: change nickname
- !wa: toggle WhatsApp mode on or off
- !die: remove the ghost from your contact list
[in this admin room]
- !csq: check modem signal quality
- !reg: check modem registration status
- !sms <num>: start an SMS conversation with a given phone number
- !wa <num>: start a WhatsApp conversation with a given phone number
- !wasetup: set up WhatsApp Web integration
- !walogon: logon to WhatsApp Web using stored credentials
- !wabridge <jid> <#channel>: bridge the WA group <jid> to an IRC channel <#channel>
- !walist: list available WA groups
- !wadel <#channel>: unbridge IRC channel <#channel>
- !wagetavatar <nick>: update the avatar for <nick>
- !wagetallavatars: update all WA avatars
[debug commands]
- !waupdateall: refresh metadata for all WA groups (primarily for debugging)
- !modemreinit: reinitialize connection to modem
- !modempath <path>: TEMPORARILY use the modem at <path>, instead of the configured one.
"#;

pub trait ControlCommon {
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand>;
    fn cf_tx(&mut self) -> &mut UnboundedSender<ContactFactoryCommand>;
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand>;
    fn send_cb_message(&mut self, msg: &str) -> Result<()>;
    fn extension_helptext() -> &'static str {
        ""
    }
    fn extension_command(&mut self, msg: Vec<&str>) -> Result<()> {
        self.unrecognised_command(msg[0])?;
        Ok(())
    }
    fn unrecognised_command(&mut self, unrec: &str) -> Result<()> {
        self.send_cb_message(&format!("Unknown command: {}", unrec))?;
        Ok(())
    }
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
            "!waupdateall" => {
                self.wa_tx().unbounded_send(WhatsappCommand::GroupUpdateAll)
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
            "!wagetavatar" => {
                if msg.get(1).is_none() {
                    self.send_cb_message("!wagetavatar takes an argument.")?;
                    return Ok(());
                }
                self.wa_tx().unbounded_send(WhatsappCommand::AvatarUpdate(msg[1].into()))
                    .unwrap();
            },
            "!wagetallavatars" => {
                self.wa_tx().unbounded_send(WhatsappCommand::AvatarUpdateAll)
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
            "!modemreinit" => {
                self.m_tx().unbounded_send(ModemCommand::ForceReinit).unwrap();
            },
            "!modempath" => {
                let arg = msg.get(1).map(|x| x.to_string());
                self.m_tx().unbounded_send(ModemCommand::UpdatePath(arg)).unwrap();
            },
            x @ "!sms" | x @ "!wa" => {
                if msg.get(1).is_none() {
                    self.send_cb_message(&format!("{} takes an argument.", x))?;
                    return Ok(());
                }
                let addr = msg[1].parse().unwrap();
                let is_wa = x == "!wa";
                self.cf_tx().unbounded_send(ContactFactoryCommand::MakeContact(addr, is_wa)).unwrap();
            },
            "!help" => {
                for line in HELPTEXT.lines() {
                    self.send_cb_message(line)?;
                }
                if Self::extension_helptext() != "" {
                    for line in Self::extension_helptext().lines() {
                        self.send_cb_message(line)?;
                    }
                }
            },
            _ => self.extension_command(msg)?
        }
        Ok(())
    }
}
