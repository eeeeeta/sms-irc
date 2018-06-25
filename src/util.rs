//! Helpful utility functions.
use huawei_modem::pdu::{PduAddress, AddressType};
use huawei_modem::convert::TryFrom;

pub type Result<T> = ::std::result::Result<T, ::failure::Error>;

pub fn normalize_address(addr: &PduAddress) -> String {
    let ton: u8 = addr.type_addr.into();
    let mut ret = format!("{:02X}", ton);
    for b in addr.number.0.iter() {
        ret += &format!("{}", b);
    }
    ret
}
pub fn un_normalize_address(addr: &str) -> Option<PduAddress> {
    if addr.len() < 3 {
        return None;
    }
    let toa = u8::from_str_radix(&addr[0..2], 16).ok()?;
    let toa = AddressType::try_from(toa).ok()?;
    let mut addr: PduAddress = addr.parse().unwrap();
    addr.number.0.remove(0);
    addr.number.0.remove(0);
    addr.type_addr = toa;
    Some(addr)
}
pub fn make_nick_for_address(addr: &PduAddress) -> String {
    let mut ret = "S".to_string();
    for ch in addr.to_string().chars() {
        match ch {
            '+' => ret.push('I'),
            '_' => ret.push('_'),
            '-' => ret.push('-'),
            '\\' => ret.push('\\'),
            '[' => ret.push('['),
            ']' => ret.push(']'),
            '{' => ret.push('{'),
            '}' => ret.push('}'),
            '^' => ret.push('^'),
            '`' => ret.push('`'),
            '|' => ret.push('|'),
            x if x.is_alphanumeric() && x.is_ascii() => ret.push(x),
            _ => ret.push('_')
        }
    }
    ret
}
