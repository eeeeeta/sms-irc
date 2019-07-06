//! IRCv3 support for IRC s2c.

pub static SUPPORTED_CAPS: &str = "away-notify";

#[derive(Clone, Copy, Debug)]
pub enum IrcCap {
    /// `away-notify` extension
    ///
    /// https://ircv3.net/specs/extensions/away-notify-3.1
    AwayNotify
}
impl IrcCap {
    pub fn cap_name(&self) -> &'static str {
        use self::IrcCap::*;

        match *self {
            AwayNotify => "away-notify"
        }
    }
    pub fn from_cap_name(cn: &str) -> Option<Self> {
        use self::IrcCap::*;

        match cn {
            "away-notify" => Some(AwayNotify),
            _ => None
        }
    }
}
