//! Common behaviours for the control bot.

use crate::comm::{WhatsappCommand, ContactFactoryCommand, ContactManagerCommand, ModemCommand};
use crate::util::Result;
use crate::models::Recipient;
use crate::admin::{InspCommand, AdminCommand, GhostCommand, GroupCommand, ContactCommand};
use crate::admin::ModemCommand as AdminModemCommand;
use crate::admin::WhatsappCommand as AdminWhatsappCommand;
use crate::models::Message;

pub trait ControlCommon {
    fn wa_send(&mut self, c: WhatsappCommand);
    fn cf_send(&mut self, c: ContactFactoryCommand);
    fn m_send(&mut self, c: ModemCommand);
    /// Process the InspIRCd-specific command specified.
    ///
    /// Returns `true` if the command was processed, or `false` if it wasn't (i.e. we aren't
    /// actually an InspIRCd link).
    fn process_insp(&mut self, _ic: InspCommand) -> Result<bool> {
        Ok(false)
    }
    /// Send a response message to the administrator.
    fn control_response(&mut self, msg: &str) -> Result<()>;
    /// Process a message sent to the control bot or user.
    fn process_admin_command(&mut self, mesg: String) -> Result<()> {
        if mesg.len() < 1 {
            return Ok(());
        }
        let msg = mesg.split(" ").collect::<Vec<_>>();
        let ac = AdminCommand::parse(&msg);
        if ac.is_none() {
            self.control_response("Invalid command. Try \x02HELP\x0f for a command listing.")?;
            return Ok(());
        }
        // FIXME: currently we just synthesise a response for the user here,
        // and send some control message. Ideally, we want to send the user's
        // command over to the target, and have that come back with a response,
        // so
        //     (a) we know it actually got done
        // and (b) it can say something useful, like "nick changed to ..."
        match ac.unwrap() {
            AdminCommand::Ghost(nick, gc) => {
                use self::GhostCommand::*;

                let mut c = None;
                match gc {
                    // This bit looks silly, because it is (see above)
                    ChangeNick(n) => {
                        c = Some(ContactManagerCommand::ChangeNick(n, Recipient::NICKSRC_USER));
                    },
                    SetWhatsapp(n) => {
                        c = Some(ContactManagerCommand::SetWhatsapp(n));
                    },
                    PresenceSubscribe => {
                        self.cf_send(ContactFactoryCommand::SubscribePresenceByNick(nick.clone()));
                    },
                    Remove => {
                        self.cf_send(ContactFactoryCommand::DropContactByNick(nick.clone()));
                    }
                }
                if let Some(c) = c {
                    self.cf_send(ContactFactoryCommand::ForwardCommandByNick(nick, c));
                }
                self.control_response("Ghost command executed.")?;
            },
            AdminCommand::Modem(mc) => {
                use self::AdminModemCommand::*;

                let cts = match mc {
                    GetCsq => ModemCommand::RequestCsq,
                    GetReg => ModemCommand::RequestReg,
                    Reinit => ModemCommand::ForceReinit,
                    TempPath(s) => ModemCommand::UpdatePath(s),
                };
                self.m_send(cts);
            },
            AdminCommand::Whatsapp(wac) => {
                use self::AdminWhatsappCommand::*;

                let cts = match wac {
                    Setup => WhatsappCommand::StartRegistration,
                    Logon => WhatsappCommand::LogonIfSaved,
                    ChatList => WhatsappCommand::GroupList,
                    UpdateAll => WhatsappCommand::GroupUpdateAll,
                    PrintAcks => WhatsappCommand::PrintAcks
                };
                self.wa_send(cts);
            },
            AdminCommand::Group(gc) => {
                use self::GroupCommand::*;

                let cts = match gc {
                    BridgeWhatsapp { jid, chan } => WhatsappCommand::GroupAssociate(jid, chan),
                    Unbridge(ch) => WhatsappCommand::GroupRemove(ch)
                };
                self.wa_send(cts);
            },
            AdminCommand::Contact(cc) => {
                use self::ContactCommand::*;
                let (addr, src) = match cc {
                    NewSms(a) => (a, Message::SOURCE_SMS),
                    NewWhatsapp(a) => (a, Message::SOURCE_WA)
                };
                self.cf_send(ContactFactoryCommand::QueryContact(addr, src));
            },
            AdminCommand::Insp(ic) => {
                if !self.process_insp(ic)? {
                    self.control_response("Error: InspIRCd link inactive!")?;
                }
            },
            AdminCommand::Help(qry) => {
                if let Some(hp) = crate::admin::help_page(qry.as_ref().map(|x| x as &str)) {
                    for line in hp.lines() {
                        self.control_response(line)?;
                    }
                }
                else {
                    self.control_response("No help page found for that topic.")?;
                }
            }
        }
        Ok(())
    }
}
