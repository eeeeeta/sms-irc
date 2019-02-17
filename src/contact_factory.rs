//! Manages the creation and maintenance of ContactManagers.

use config::Config;
use store::Store;
use comm::{ContactFactoryCommand, ContactManagerCommand, ChannelMaker, InitParameters};
use futures::{Future, Async, Poll, Stream};
use futures::sync::mpsc::UnboundedReceiver;
use std::collections::{HashMap, HashSet};
use tokio_core::reactor::Handle;
use huawei_modem::pdu::PduAddress;
use contact::ContactManager;
use util::{self, Result};
use models::Recipient;
use tokio_timer::Interval;
use failure::Error;
use contact_common::ContactManagerManager;
use config::IrcClientConfig;

pub struct ContactFactory {
    rx: UnboundedReceiver<ContactFactoryCommand>,
    contacts_starting: HashMap<PduAddress, Box<Future<Item = ContactManager, Error = Error>>>,
    contacts: HashMap<PduAddress, ContactManager>,
    contacts_presence: HashMap<PduAddress, Option<String>>,
    failed_contacts: HashSet<PduAddress>,
    failure_int: Interval,
    messages_processed: HashSet<i32>,
    cfg: Config,
    store: Store,
    cm: ChannelMaker,
    hdl: Handle
}
impl Future for ContactFactory {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(res) = self.rx.poll().unwrap() {
            use self::ContactFactoryCommand::*;

            let msg = res.expect("contactfactory rx died");
            match msg {
                ProcessMessages => self.process_messages()?,
                ProcessGroups => self.process_groups()?,
                LoadRecipients => self.load_recipients()?,
                MakeContact(addr, wa) => self.make_contact(addr, wa)?,
                DropContact(addr) => self.drop_contact(addr)?,
                UpdateAway(addr, away) => self.update_away(addr, away),
                ProcessAvatars => {
                    // FIXME: implement
                }
            }
        }
        let mut to_remove = vec![];
        for (addr, fut) in self.contacts_starting.iter_mut() {
            match fut.poll() {
                Ok(Async::Ready(c)) => {
                    if let Some(pre) = self.contacts_presence.get(&addr) {
                        c.add_command(ContactManagerCommand::UpdateAway(pre.clone()));
                    }
                    self.contacts.insert(addr.clone(), c);
                    to_remove.push(addr.clone())
                },
                Ok(Async::NotReady) => {},
                Err(e) => {
                    warn!("Making contact for {} failed: {}", addr, e);
                    self.failed_contacts.insert(addr.clone());
                    to_remove.push(addr.clone())
                }
            }
        }
        for tr in to_remove {
            self.contacts_starting.remove(&tr);
        }
        let mut to_remove = vec![];
        for (addr, fut) in self.contacts.iter_mut() {
            match fut.poll() {
                Ok(Async::Ready(_)) => unreachable!(),
                Ok(Async::NotReady) => {},
                Err(e) => {
                    warn!("Contact for {} failed: {}", addr, e);
                    self.failed_contacts.insert(addr.clone());
                    to_remove.push(addr.clone())
                }
            }
        }
        for tr in to_remove {
            self.contacts.remove(&tr);
        }
        while let Async::Ready(_) = self.failure_int.poll()? {
            self.process_failures()?;
        }
        Ok(Async::NotReady)
    }
}
impl ContactManagerManager for ContactFactory {
    fn setup_contact_for(&mut self, recip: Recipient, addr: PduAddress) -> Result<()> {
        let cfut = {
            let ip = self.get_init_parameters();
            ContactManager::new(recip, ip)
        };
        self.contacts_starting.insert(addr, Box::new(cfut));
        Ok(())
    }
    fn remove_contact_for(&mut self, addr: &PduAddress) -> Result<()> {
        self.contacts.remove(addr);
        Ok(())
    }
    fn has_contact(&mut self, addr: &PduAddress) -> bool {
        self.contacts_starting.get(addr).is_some() || self.contacts.get(addr).is_some()
    }
    fn store(&mut self) -> &mut Store {
        &mut self.store
    }
}
impl ContactFactory {
    pub fn new(cfg: Config, store: Store, mut cm: ChannelMaker, hdl: Handle) -> Self {
        use std::time::{Instant, Duration};

        let rx = cm.cf_rx.take().unwrap();
        cm.cf_tx.unbounded_send(ContactFactoryCommand::LoadRecipients).unwrap();
        let failure_int = Interval::new(Instant::now(), Duration::from_millis(cfg.client.as_ref().unwrap().failure_interval.unwrap_or(30000)));
        Self {
            rx, failure_int,
            contacts_starting: HashMap::new(),
            contacts_presence: HashMap::new(),
            contacts: HashMap::new(),
            failed_contacts: HashSet::new(),
            messages_processed: HashSet::new(),
            cfg, store, cm, hdl
        }
    }
    fn process_failures(&mut self) -> Result<()> {
        for addr in ::std::mem::replace(&mut self.failed_contacts, HashSet::new()) {
            self.make_contact(addr, false)?;
        }
        Ok(())
    }
    fn get_init_parameters(&mut self) -> InitParameters<IrcClientConfig> {
        InitParameters {
            cfg: &self.cfg,
            cfg2: self.cfg.client.as_ref().unwrap(),
            store: self.store.clone(),
            cm: &mut self.cm,
            hdl: &self.hdl
        }
    }
    fn load_recipients(&mut self) -> Result<()> {
        for recip in self.store.get_all_recipients()? {
            self.setup_recipient(recip)?;
        }
        Ok(())
    }
    fn update_away(&mut self, addr: PduAddress, away: Option<String>) {
        if let Some(c) = self.contacts.get(&addr) {
            c.add_command(ContactManagerCommand::UpdateAway(away.clone()));
        }
        self.contacts_presence.insert(addr, away);
    }
    fn process_groups(&mut self) -> Result<()> {
        debug!("Processing group updates");
        for (_, c) in self.contacts.iter_mut() {
            c.add_command(ContactManagerCommand::ProcessGroups);
        }
        Ok(())
    }
    fn process_messages(&mut self) -> Result<()> {
        for msg in self.store.get_all_messages()? {
            if self.messages_processed.insert(msg.id) {
                let addr = util::un_normalize_address(&msg.phone_number)
                    .ok_or(format_err!("invalid address {} in db", msg.phone_number))?;
                if self.contacts_starting.get(&addr).is_some() {
                    continue;
                }
                if let Some(c) = self.contacts.get_mut(&addr) {
                    c.add_command(ContactManagerCommand::ProcessMessages);
                    continue;
                }
                self.make_contact(addr, msg.text.is_some())?;
            }
        }
        Ok(())
    }
}
