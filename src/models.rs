use schema::{recipients, messages};

#[derive(Queryable)]
pub struct Recipient {
    pub id: i32,
    pub phone_number: String,
    pub nick: String
}
#[derive(Insertable)]
#[table_name="recipients"]
pub struct NewRecipient<'a> {
    pub phone_number: &'a str,
    pub nick: &'a str
}
#[derive(Queryable, Debug)]
pub struct Message {
    pub id: i32,
    pub phone_number: String,
    pub pdu: Vec<u8>,
    pub csms_data: Option<i32>
}
#[derive(Insertable)]
#[table_name="messages"]
pub struct NewMessage<'a> {
    pub phone_number: &'a str,
    pub pdu: &'a [u8],
    pub csms_data: Option<i32>
}
