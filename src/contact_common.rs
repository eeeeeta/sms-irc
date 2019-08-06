//! Shared behaviour for contact factories/stores.

use huawei_modem::pdu::PduAddress;
use crate::models::{Recipient, Message};
use crate::store::Store;
use crate::comm::ContactManagerCommand;
use crate::util::{self, Result};
use futures::sync::mpsc::UnboundedSender;
use crate::comm::{WhatsappCommand, ModemCommand, ControlBotCommand};

pub trait ContactManagerManager {
    fn wa_tx(&mut self) -> &mut UnboundedSender<WhatsappCommand>;
    fn m_tx(&mut self) -> &mut UnboundedSender<ModemCommand>;
    fn cb_tx(&mut self) -> &mut UnboundedSender<ControlBotCommand>;
    fn setup_contact_for(&mut self, _: Recipient, _: PduAddress) -> Result<()>;
    fn remove_contact_for(&mut self, _: &PduAddress) -> Result<()>;
    fn has_contact(&mut self, _: &PduAddress) -> bool;
    fn store(&mut self) -> &mut Store;
    fn forward_cmd(&mut self, _: &PduAddress, _: ContactManagerCommand) -> Result<()>;
    fn resolve_nick(&self, _: &str) -> Option<PduAddress>;
    fn setup_recipient(&mut self, recip: Recipient) -> Result<()> {
        let addr = util::un_normalize_address(&recip.phone_number)
            .ok_or(format_err!("invalid num {} in db", recip.phone_number))?;
        debug!("Setting up recipient for {} (nick {})", addr, recip.nick);
        if self.has_contact(&addr) {
            debug!("Not doing anything; contact already exists");
            return Ok(());
        }
        self.setup_contact_for(recip, addr)?;
        Ok(())
    }
    fn query_contact(&mut self, a: PduAddress, src: i32) -> Result<()> {
        if let Some(recip) = self.store().get_recipient_by_addr_opt(&a)? {
            self.cb_tx().unbounded_send(
                ControlBotCommand::CommandResponse(
                    format!("Ghost exists already; nickname is `{}`.", recip.nick)
                    ))
                .unwrap();
            self.setup_recipient(recip)?;
        }
        else {
            self.cb_tx().unbounded_send(
                ControlBotCommand::CommandResponse(
                    format!("Setting up a ghost for number `{}`...", a) 
                    ))
                .unwrap();
            self.request_contact(a, src)?;
        }
        Ok(())
    }
    fn setup_contact(&mut self, a: PduAddress) -> Result<()> {
        if let Some(recip) = self.store().get_recipient_by_addr_opt(&a)? {
            self.setup_recipient(recip)?;
        }
        else {
            error!("Attempted to setup non-existent recipient {}", a);
        }
        Ok(())
    }
    fn request_contact(&mut self, a: PduAddress, src: i32) -> Result<bool> {
        if let Some(recip) = self.store().get_recipient_by_addr_opt(&a)? {
            self.setup_recipient(recip)?;
            Ok(true)
        }
        else {
            info!("No contact exists yet for {}; asking for its creation", a);
            match src {
                Message::SOURCE_SMS => {
                    self.m_tx().unbounded_send(ModemCommand::MakeContact(a))
                        .unwrap();
                },
                Message::SOURCE_WA => {
                    self.wa_tx().unbounded_send(WhatsappCommand::MakeContact(a))
                        .unwrap();
                },
                _ => {
                    error!("Contact requested for unknown message source {}", src);
                }
            }
            Ok(false)
        }
    }
    fn drop_contact(&mut self, addr: PduAddress) -> Result<()> {
        info!("Dropping contact {}", addr);
        self.store().delete_recipient_with_addr(&addr)?;
        self.remove_contact_for(&addr)?;
        Ok(())
    }
    fn subscribe_presence_by_nick(&mut self, nick: String) {
        if let Some(a) = self.resolve_nick(&nick) {
            self.wa_tx().unbounded_send(WhatsappCommand::SubscribePresence(a))
                .unwrap();
        }
        else {
            warn!("Tried to subscribe presence for nonexistent nick {}", nick);
        }
    }
    fn drop_contact_by_nick(&mut self, nick: String) -> Result<()> {
        if let Some(a) = self.resolve_nick(&nick) {
            self.drop_contact(a)?;
        }
        else {
            warn!("Tried to drop nonexistent contact with nick {}", nick);
        }
        Ok(())
    }
    fn forward_cmd_by_nick(&mut self, nick: &str, cmd: ContactManagerCommand) -> Result<()> {
        if let Some(a) = self.resolve_nick(nick) {
            self.forward_cmd(&a, cmd)?;
        }
        else {
            warn!("Could not resolve forwarded command intended for {}", nick);
        }
        Ok(())
    }
}
