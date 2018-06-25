//! Helpful utility functions.
use huawei_modem::pdu::{PduAddress, AddressType, HexData, PhoneNumber};
use huawei_modem::convert::TryFrom;

pub type Result<T> = ::std::result::Result<T, ::failure::Error>;

pub fn normalize_address(addr: &PduAddress) -> String {
    let ton: u8 = addr.type_addr.into();
    let mut ret = format!("{:02X}", ton);
    ret += &HexData(&addr.number.0).to_string();
    ret
}
pub fn un_normalize_address(addr: &str) -> Option<PduAddress> {
    if addr.len() < 3 {
        return None;
    }
    let mut data = HexData::decode(addr).ok()?;
    let toa = data.remove(0);
    let toa = AddressType::try_from(toa).ok()?;
    let num = PhoneNumber(data);
    Some(PduAddress {
        type_addr: toa,
        number: num
    })
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
            x if x.is_ascii_alphanumeric() => ret.push(x),
            _ => ret.push('_')
        }
    }
    ret
}
