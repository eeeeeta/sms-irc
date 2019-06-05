//! Handles database stuff.

use diesel::PgConnection;
use r2d2_diesel::ConnectionManager;
use r2d2::Pool;
use crate::config::Config;
use std::sync::Arc;
use huawei_modem::pdu::PduAddress;
use diesel::prelude::*;
use serde_json;
use whatsappweb::connection::PersistentSession;
use whatsappweb::Jid;
use crate::util::{self, Result};
use crate::models::*;

#[derive(Clone)]
pub struct Store {
    inner: Arc<Pool<ConnectionManager<PgConnection>>>
}
impl Store {
    pub fn new(cfg: &Config) -> Result<Self> {
        let manager = ConnectionManager::new(cfg.database_url.clone());
        let pool = Pool::builder()
            .build(manager)?;
        Ok(Self {
            inner: Arc::new(pool)
        })
    }
    pub fn store_message(&mut self, addr: &PduAddress, pdu: &[u8], csms_data: Option<i32>) -> Result<Message> {
        use crate::schema::messages;

        let num = util::normalize_address(addr);
        let nm = NewMessage {
            phone_number: &num,
            pdu,
            csms_data
        };
        let conn = self.inner.get()?;

        let res = ::diesel::insert_into(messages::table)
            .values(&nm)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn store_plain_message(&mut self, addr: &PduAddress, text: &str, group_target: Option<i32>) -> Result<Message> {
        use crate::schema::messages;

        let num = util::normalize_address(addr);
        let nm = NewPlainMessage {
            phone_number: &num,
            text,
            group_target
        };
        let conn = self.inner.get()?;

        let res = ::diesel::insert_into(messages::table)
            .values(&nm)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn store_wa_persistence(&mut self, p: PersistentSession) -> Result<()> {
        use crate::schema::wa_persistence;
        use crate::schema::wa_persistence::dsl::*;
        
        let pdata = serde_json::to_value(&p)?;
        let pdata = PersistenceData {
            rev: 0,
            data: pdata
        };
        let conn = self.inner.get()?;

        ::diesel::insert_into(wa_persistence::table)
            .values(&pdata)
            .on_conflict(rev)
            .do_update()
            .set(data.eq(::diesel::pg::upsert::excluded(data)))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn store_group(&mut self, jid: &Jid, channel: &str, participants: Vec<i32>, admins: Vec<i32>, topic: &str) -> Result<Group> {
        use crate::schema::groups;
        let jid = jid.to_string();
        let newgrp = NewGroup {
            jid: &jid,
            channel, participants, admins, topic
        };
        let conn = self.inner.get()?;

        let res = ::diesel::insert_into(groups::table)
            .values(&newgrp)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn update_group(&mut self, j: &Jid, parts: Vec<i32>, adms: Vec<i32>, tpc: &str) -> Result<Group> {
        use crate::schema::groups::dsl::*;
        let j = j.to_string();
        let conn = self.inner.get()?;

        let res = ::diesel::update(groups.filter(jid.eq(&j)))
            .set((participants.eq(parts), admins.eq(adms), topic.eq(tpc)))
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn get_wa_persistence_opt(&mut self) -> Result<Option<PersistentSession>> {
        use crate::schema::wa_persistence::dsl::*;
        let conn = self.inner.get()?;
        let res: Option<PersistenceData> = wa_persistence.filter(rev.eq(0))
            .first(&*conn)
            .optional()?;
        let res = match res {
            Some(res) => {
                let res: PersistentSession = serde_json::from_value(res.data)?;
                Some(res)
            },
            None => None
        };
        Ok(res)
    }
    pub fn store_recipient(&mut self, addr: &PduAddress, nick: &str, wa: bool) -> Result<Recipient> {
        use crate::schema::recipients;

        let num = util::normalize_address(addr);
        let nr = NewRecipient {
            phone_number: &num,
            nick,
            whatsapp: wa,
            avatar_url: None
        };
        let conn = self.inner.get()?;

        let res = ::diesel::insert_into(recipients::table)
            .values(&nr)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn update_recipient_nick(&mut self, addr: &PduAddress, n: &str) -> Result<()> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        ::diesel::update(recipients)
            .filter(phone_number.eq(num))
            .set(nick.eq(n))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn update_recipient_avatar_url(&mut self, addr: &PduAddress, a: Option<String>) -> Result<()> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        ::diesel::update(recipients)
            .filter(phone_number.eq(num))
            .set(avatar_url.eq(a))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn update_recipient_wa(&mut self, addr: &PduAddress, wa: bool) -> Result<()> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        ::diesel::update(recipients)
            .filter(phone_number.eq(num))
            .set(whatsapp.eq(wa))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn get_recipient_by_id_opt(&mut self, i: i32) -> Result<Option<Recipient>> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let res = recipients.filter(id.eq(i))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_recipient_by_nick_opt(&mut self, n: &str) -> Result<Option<Recipient>> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let res = recipients.filter(nick.eq(n))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_recipient_by_addr_opt(&mut self, addr: &PduAddress) -> Result<Option<Recipient>> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let num = util::normalize_address(addr);
        let res = recipients.filter(phone_number.eq(num))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_all_recipients(&mut self) -> Result<Vec<Recipient>> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let res = recipients
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_all_messages(&mut self) -> Result<Vec<Message>> {
        use crate::schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let res = messages
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_group_by_id(&mut self, gid: i32) -> Result<Group> {
        use crate::schema::groups::dsl::*;
        let conn = self.inner.get()?;

        let res = groups.filter(id.eq(gid))
            .first(&*conn)?;
        Ok(res)
    }
    pub fn get_all_groups(&mut self) -> Result<Vec<Group>> {
        use crate::schema::groups::dsl::*;
        let conn = self.inner.get()?;

        let res = groups
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_group_by_jid_opt(&mut self, j: &Jid) -> Result<Option<Group>> {
        use crate::schema::groups::dsl::*;
        let j = j.to_string();
        let conn = self.inner.get()?;

        let res = groups.filter(jid.eq(j))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_group_by_chan_opt(&mut self, c: &str) -> Result<Option<Group>> {
        use crate::schema::groups::dsl::*;
        let conn = self.inner.get()?;

        let res = groups.filter(channel.eq(c))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_groups_for_recipient(&mut self, addr: &PduAddress) -> Result<Vec<Group>> {
        use crate::schema::groups::dsl::*;
        let r = self.get_recipient_by_addr_opt(addr)?
            .ok_or(format_err!("get_groups_for_recipient couldn't find recipient"))?;
        let conn = self.inner.get()?;

        let res = groups.filter(participants.contains(vec![r.id]))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_messages_for_recipient(&mut self, addr: &PduAddress) -> Result<Vec<Message>> {
        use crate::schema::messages::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        let res = messages.filter(phone_number.eq(num))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_all_concatenated(&mut self, num: &str, rf: i32) -> Result<Vec<Message>> {
        use crate::schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let res = messages.filter(csms_data.eq(rf)
                                  .and(phone_number.eq(num)))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn delete_group_with_id(&mut self, i: i32) -> Result<()> {
        use crate::schema::groups::dsl::*;
        let conn = self.inner.get()?;

        let rows_affected = ::diesel::delete(groups.filter(id.eq(i)))
            .execute(&*conn)?;
        if rows_affected == 0 {
            return Err(format_err!("no rows affected deleting gid {}", i));
        }
        Ok(())
    }
    pub fn delete_recipient_with_addr(&mut self, addr: &PduAddress) -> Result<()> {
        use crate::schema::recipients::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        let rows_affected = ::diesel::delete(recipients.filter(phone_number.eq(num)))
            .execute(&*conn)?;
        if rows_affected == 0 {
            return Err(format_err!("no rows affected deleting recip {}", addr));
        }
        Ok(())
    }
    pub fn delete_message(&mut self, mid: i32) -> Result<()> {
        use crate::schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let rows_affected = ::diesel::delete(messages.filter(id.eq(mid)))
            .execute(&*conn)?;
        if rows_affected == 0 {
            return Err(format_err!("no rows affected deleting mid #{}", mid));
        }
        Ok(())
    }
}
