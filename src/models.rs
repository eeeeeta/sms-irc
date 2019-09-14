use crate::schema::{recipients, messages, groups, wa_persistence, wa_msgids};
use serde_json::Value;
use chrono::NaiveDateTime;
use huawei_modem::pdu::PduAddress;
use crate::util::{self, Result};

#[derive(Queryable)]
pub struct Recipient {
    pub id: i32,
    pub phone_number: String,
    pub nick: String,
    pub whatsapp: bool,
    pub avatar_url: Option<String>,
    pub notify: Option<String>,
    pub nicksrc: i32,
}
impl Recipient {
    /// Nick source: migrated from previous sms-irc install
    pub const NICKSRC_MIGRATED: i32 = -1;
    /// Nick source: autogenerated from phone number
    pub const NICKSRC_AUTO: i32 = 0;
    /// Nick source: renamed by user
    pub const NICKSRC_USER: i32 = 1;
    /// Nick source: from WhatsApp contact name
    pub const NICKSRC_WA_CONTACT: i32 = 2;
    /// Nick source: from WhatsApp notify
    pub const NICKSRC_WA_NOTIFY: i32 = 3;
    /// Nick source: from a nick collision
    pub const NICKSRC_COLLISION: i32 = 4;
    pub fn get_addr(&self) -> Result<PduAddress> {
        let addr = util::un_normalize_address(&self.phone_number)
            .ok_or(format_err!("invalid address {} in db", self.phone_number))?;
        Ok(addr)
    }
}
#[derive(Insertable)]
#[table_name="recipients"]
pub struct NewRecipient<'a> {
    pub phone_number: &'a str,
    pub nick: &'a str,
    pub whatsapp: bool,
    pub avatar_url: Option<&'a str>,
    pub notify: Option<&'a str>,
    pub nicksrc: i32
}
#[derive(Queryable, Debug)]
pub struct Message {
    pub id: i32,
    pub phone_number: String,
    pub pdu: Option<Vec<u8>>,
    pub csms_data: Option<i32>,
    pub group_target: Option<i32>,
    pub text: Option<String>,
    pub source: i32,
    pub ts: NaiveDateTime
}
impl Message {
    pub const SOURCE_SMS: i32 = 0;
    pub const SOURCE_WA: i32 = 1;

    pub fn get_addr(&self) -> Result<PduAddress> {
        let addr = util::un_normalize_address(&self.phone_number)
            .ok_or(format_err!("invalid address {} in db", self.phone_number))?;
        Ok(addr)
    }
}
#[derive(Queryable, Debug)]
pub struct Group {
    pub id: i32,
    pub jid: String,
    pub channel: String,
    pub participants: Vec<i32>,
    pub admins: Vec<i32>,
    pub topic: String
}
#[derive(Insertable, Queryable, Debug)]
#[table_name="wa_persistence"]
pub struct PersistenceData {
    pub rev: i32,
    pub data: Value
}
#[derive(Insertable, Queryable, Debug)]
#[table_name="wa_msgids"]
pub struct WaMessageId {
    pub mid: String
}
#[derive(Insertable)]
#[table_name="groups"]
pub struct NewGroup<'a> {
    pub jid: &'a str,
    pub channel: &'a str,
    pub participants: Vec<i32>,
    pub admins: Vec<i32>,
    pub topic: &'a str
}
#[derive(Insertable)]
#[table_name="messages"]
pub struct NewMessage<'a> {
    pub phone_number: &'a str,
    pub pdu: &'a [u8],
    pub csms_data: Option<i32>,
    pub source: i32,
}
#[derive(Insertable)]
#[table_name="messages"]
pub struct NewPlainMessage<'a> {
    pub phone_number: &'a str,
    pub group_target: Option<i32>,
    pub text: &'a str,
    pub source: i32,
    pub ts: NaiveDateTime
}
