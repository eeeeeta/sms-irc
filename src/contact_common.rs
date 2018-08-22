//! Shared behaviour for contact factories/stores.

use huawei_modem::pdu::PduAddress;
use models::Recipient;
use store::Store;
use util::{self, Result};

pub trait ContactManagerManager {
    fn setup_contact_for(&mut self, _: Recipient, _: PduAddress) -> Result<()>;
    fn remove_contact_for(&mut self, _: &PduAddress) -> Result<()>;
    fn has_contact(&mut self, _: &PduAddress) -> bool;
    fn store(&mut self) -> &mut Store;
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
    fn make_contact(&mut self, addr: PduAddress) -> Result<()> {
        if let Some(recip) = self.store().get_recipient_by_addr_opt(&addr)? {
            self.setup_recipient(recip)?;
        }
        else {
            let nick = util::make_nick_for_address(&addr);
            info!("Creating new recipient for {} (nick {})", addr, nick);
            let recip = self.store().store_recipient(&addr, &nick, false)?;
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
}
