//! InspIRCd user tracking.

use util::{self, Result};
use huawei_modem::pdu::PduAddress;

#[derive(Debug)]
pub struct InspUser {
    pub ts: i64,
    pub nick: String,
    pub hostname: String,
    pub displayed_hostname: String,
    pub ident: String,
    pub ip: String,
    pub signon_time: i64,
    // modes in +abc [args] format
    pub modes: String,
    pub gecos: String,
}
impl InspUser {
    pub fn new_from_uid_line(args: Vec<String>, suffix: Option<String>) -> Result<(String, Self)> {
        if args.len() < 9 {
            Err(format_err!("UID with invalid args: {:?}", args))?
        }
        if suffix.is_none() {
            Err(format_err!("UID with no suffix"))?
        }
        let mut args = args.into_iter();
        let uuid = args.next().unwrap();
        let ret = Self {
            ts: args.next().unwrap().parse()?,
            nick: args.next().unwrap(),
            hostname: args.next().unwrap(),
            displayed_hostname: args.next().unwrap(),
            ident: args.next().unwrap(),
            ip: args.next().unwrap(),
            signon_time: args.next().unwrap().parse()?,
            modes: args.collect::<Vec<_>>().join(" "),
            gecos: suffix.unwrap()
        };
        Ok((uuid, ret))
    }
    pub fn new_for_controlbot(nick: String, hostname: &str) -> Self {
        use chrono::Utc;

        let ts = Utc::now().timestamp();
        Self {
            ts, nick,
            hostname: hostname.into(),
            displayed_hostname: hostname.into(),
            ident: "~control".into(),
            ip: "0.0.0.0".into(),
            signon_time: ts,
            modes: "+i".into(),
            gecos: "sms-irc control bot".into()
        }
    }
    pub fn new_from_recipient(a: PduAddress, nick: String, hostname: &str) -> Self {
        use chrono::Utc;

        let ts = Utc::now().timestamp();
        let safe_addr = util::string_to_irc_nick(&a.to_string());
        Self {
            ts, nick,
            hostname: hostname.into(),
            displayed_hostname: hostname.into(),
            ident: safe_addr,
            ip: "0.0.0.0".into(),
            signon_time: ts,
            modes: "+i".into(),
            gecos: format!("{}", a)
        }
    }
}

