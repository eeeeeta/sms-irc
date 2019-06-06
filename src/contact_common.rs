//! Shared behaviour for contact factories/stores.

use huawei_modem::pdu::PduAddress;
use crate::models::Recipient;
use crate::store::Store;
use crate::comm::ContactManagerCommand;
use crate::util::{self, Result};

pub trait ContactManagerManager {
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
    fn make_contact(&mut self, addr: PduAddress, is_wa: bool) -> Result<()> {
        if let Some(recip) = self.store().get_recipient_by_addr_opt(&addr)? {
            self.setup_recipient(recip)?;
        }
        else {
            let nick = util::make_nick_for_address(&addr);
            let watext = if is_wa { "WA recipient"} else { "recipient" };
            info!("Creating new {} for {} (nick {})", watext, addr, nick);
            let recip = self.store().store_recipient(&addr, &nick, is_wa)?;
            self.setup_recipient(recip)?;
        }
        Ok(())
    }
    fn drop_contact(&mut self, addr: PduAddress) -> Result<()> {
        info!("Dropping contact {}", addr);
        self.store().delete_recipient_with_addr(&addr)?;
        self.remove_contact_for(&addr)?;
        Ok(())
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
