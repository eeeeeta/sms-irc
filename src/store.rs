//! Handles database stuff.

use diesel::PgConnection;
use r2d2_diesel::ConnectionManager;
use r2d2::Pool;
use config::Config;
use std::sync::Arc;
use huawei_modem::pdu::PduAddress;
use diesel::prelude::*;
use util::{self, Result};
use models::*;

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
        use schema::messages;

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
    pub fn store_recipient(&mut self, addr: &PduAddress, nick: &str) -> Result<Recipient> {
        use schema::recipients;

        let num = util::normalize_address(addr);
        let nr = NewRecipient {
            phone_number: &num,
            nick
        };
        let conn = self.inner.get()?;

        let res = ::diesel::insert_into(recipients::table)
            .values(&nr)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn update_recipient_nick(&mut self, addr: &PduAddress, n: &str) -> Result<()> {
        use schema::recipients::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        ::diesel::update(recipients)
            .filter(phone_number.eq(num))
            .set(nick.eq(n))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn get_recipient_by_addr_opt(&mut self, addr: &PduAddress) -> Result<Option<Recipient>> {
        use schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let num = util::normalize_address(addr);
        let res = recipients.filter(phone_number.eq(num))
            .first(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_all_recipients(&mut self) -> Result<Vec<Recipient>> {
        use schema::recipients::dsl::*;
        let conn = self.inner.get()?;

        let res = recipients
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_all_messages(&mut self) -> Result<Vec<Message>> {
        use schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let res = messages
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_messages_for_recipient(&mut self, addr: &PduAddress) -> Result<Vec<Message>> {
        use schema::messages::dsl::*;
        let conn = self.inner.get()?;
        let num = util::normalize_address(addr);

        let res = messages.filter(phone_number.eq(num))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_all_concatenated(&mut self, num: &str, rf: i32) -> Result<Vec<Message>> {
        use schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let res = messages.filter(csms_data.eq(rf)
                                  .and(phone_number.eq(num)))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn delete_recipient_with_addr(&mut self, addr: &PduAddress) -> Result<()> {
        use schema::recipients::dsl::*;
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
        use schema::messages::dsl::*;
        let conn = self.inner.get()?;

        let rows_affected = ::diesel::delete(messages.filter(id.eq(mid)))
            .execute(&*conn)?;
        if rows_affected == 0 {
            return Err(format_err!("no rows affected deleting mid #{}", mid));
        }
        Ok(())
    }
}
