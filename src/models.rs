use crate::schema::{recipients, messages, groups, wa_persistence, wa_msgids};
use serde_json::Value;

#[derive(Queryable)]
pub struct Recipient {
    pub id: i32,
    pub phone_number: String,
    pub nick: String,
    pub whatsapp: bool,
    pub avatar_url: Option<String>,
    pub notify: Option<String>,
}
#[derive(Insertable)]
#[table_name="recipients"]
pub struct NewRecipient<'a> {
    pub phone_number: &'a str,
    pub nick: &'a str,
    pub whatsapp: bool,
    pub avatar_url: Option<&'a str>,
    pub notify: Option<&'a str>
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
}
impl Message {
    pub const SOURCE_SMS: i32 = 0;
    pub const SOURCE_WA: i32 = 1;
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
    pub source: i32
}
#[derive(Insertable)]
#[table_name="messages"]
pub struct NewPlainMessage<'a> {
    pub phone_number: &'a str,
    pub group_target: Option<i32>,
    pub text: &'a str,
    pub source: i32
}
