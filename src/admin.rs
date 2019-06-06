//! Administration UI (new).

use huawei_modem::pdu::PduAddress;
use whatsappweb::Jid;

macro_rules! extract_command {
    ($inp:ident, $cmd:ident, $rest:ident) => {
        if $inp.len() < 1 {
            return None;
        }
        let ($cmd, $rest) = (&$inp[0], &$inp[1..]);
        let $cmd = $cmd.to_lowercase();
        let $cmd = &$cmd as &str;
    }
}
pub enum GhostCommand {
    ChangeNick(String),
    SetWhatsapp(bool),
    Remove
}
impl GhostCommand {
    pub fn help() -> &'static str {
        "\x02*** GHOST subcommand ***\x0f
The following commands are available:
\x02RENAME\x0f \x1dnew_nick\x0f
    Change the ghost's nickname to \x1dnew_nick\x0f.
\x02WHATSAPP\x0f \x1dtrue|false\x0f
    Enable or disable WhatsApp mode for this recipient.
\x02REMOVE\x0f \x0307(aliases \x02KILL\x02, \x02DIE\x02)\x0f
    Remove this recipient, causing them to disconnect.
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            ("rename", &[new_nick]) => {
                Some(GhostCommand::ChangeNick(new_nick.to_owned()))
            },
            ("whatsapp", &[value]) => {
                if let Ok(b) = value.parse() {
                    Some(GhostCommand::SetWhatsapp(b))
                }
                else {
                    None
                }
            },
            ("die", _) | ("kill", _) | ("remove", _) => {
                Some(GhostCommand::Remove)
            },
            _ => None
        }
    }
}
pub enum ModemCommand {
    GetCsq,
    GetReg,
    Reinit,
    TempPath(Option<String>)
}
impl ModemCommand {
    pub fn help() -> &'static str {
        "\x02*** MODEM subcommand ***\x0f
The following commands are available:
\x02SIGNAL\x0f \x0307(alias \x02CSQ\x02)\x0f
    Get the current signal strength, as reported by the modem.
\x02REGISTRATION\x0f \x0307(alias \x02REG\x02)\x0f
    Get the modem's registration state.
\x02RESTART\x0f
    Reinitialize the connection to the modem.
\x02PATH\x0f \x1d[temp_path]\x0f
    \x1f\x02Temporarily\x0f sets the modem path to \x1dtemp_path\x0f.
    Not specifying a value for \x1dtemp_path\x0f will disable the modem.
    \x1fTip: Change the \x11modem_path\x11 option in the configuration to make this permanent.\x1f
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            ("signal", _) | ("csq", _) => Some(ModemCommand::GetCsq),
            ("registration", _) | ("reg", _) => Some(ModemCommand::GetReg),
            ("restart", _) => Some(ModemCommand::Reinit),
            ("path", &[path]) => {
                Some(ModemCommand::TempPath(Some(path.to_owned())))
            },
            ("path", _) => Some(ModemCommand::TempPath(None)),
            _ => None
        }
    }
}
pub enum ContactCommand {
    NewSms(PduAddress),
    NewWhatsapp(PduAddress),
}
impl ContactCommand {
    pub fn help() -> &'static str {
       "\x02*** CONTACT subcommand ***\x0f
The following commands are available:
\x02WHATSAPP\x0f \x1dnumber\x0f
    Contact someone new, with the phone number \x1dnumber\x0f, via WhatsApp.
\x02SMS\x0f \x1dnumber\x0f
    Contact someone new, with the phone number \x1dnumber\x0f, via SMS.
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            (x @ "whatsapp", &[num]) | (x @ "sms", &[num]) => {
                if let Ok(num) = num.parse() {
                    if x == "whatsapp" {
                        Some(ContactCommand::NewWhatsapp(num))
                    }
                    else {
                        Some(ContactCommand::NewSms(num))
                    }
                }
                else {
                    None
                }
            },
            _ => None
        }
    }
}
pub enum WhatsappCommand {
    Setup,
    Logon,
    ChatList,
    UpdateAll,
}
impl WhatsappCommand {
    pub fn help() -> &'static str {
       "\x02*** WHATSAPP subcommand ***\x0f
The following commands are available:
\x02SETUP\x0f
    Begin the WhatsApp registration process. This requires you to have your phone ready, in order to scan a QR code.
    \x1f\x02Warning:\x02 This command will \x02erase\x02 your existing WhatsApp credentials.\x1f
\x02LOGON\x0f
    Log on to WhatsApp Web using stored credentials.
    This command will usually not be required, but is helpful if the bridge appears stuck.
\x02CHATS\x0f
    List all WhatsApp chats you're currently part of.
\x02REBUILD\x0f
    Refresh group metadata for all WhatsApp groups you're currently in.
    This command will usually not be required, and is mainly useful for debugging.
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            ("setup", _) => Some(WhatsappCommand::Setup),
            ("logon", _) => Some(WhatsappCommand::Logon),
            ("chats", _) => Some(WhatsappCommand::ChatList),
            ("rebuild", _) => Some(WhatsappCommand::UpdateAll),
            _ => None
        }
    }
}
pub enum GroupCommand {
    BridgeWhatsapp {
        jid: Jid,
        chan: String
    },
    Unbridge(String)
}
impl GroupCommand {
    pub fn help() -> &'static str {
       "\x02*** GROUP subcommand ***\x0f
The following commands are available:
\x02BRIDGE\x0f \x1djid\x0f \x1dchan\x0f
    Bridges the WhatsApp groupchat with JID \x1djid\x0f to the IRC channel \x1dchan\x0f.
\x02UNBRIDGE\x0f \x1dchan\x0f
    Unbridges the IRC channel \x1dchan\x0f.
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            ("bridge", &[jid, chan]) => {
                if let Ok(j) = jid.parse() {
                    if chan.starts_with("#") {
                        return Some(GroupCommand::BridgeWhatsapp {
                            jid: j,
                            chan: chan.to_owned()
                        });
                    }
                }
                None
            },
            ("unbridge", &[chan]) => {
                if chan.starts_with("#") {
                    Some(GroupCommand::Unbridge(chan.to_owned()))
                }
                else {
                    None
                }
            },
            _ => None
        }
    }
}
pub enum InspCommand {
    Raw(String),
    QueryUuid(String),
    QueryNickUuid(String)
}
impl InspCommand {
    pub fn help() -> &'static str {
       "\x02*** INSP subcommand ***\x0f
The following commands are available:
\x02RAW\x0f \x1draw IRC line\x0f
    Sends the \x1draw IRC line\x0f to the linked IRC server.
    \x1f\x02Warning:\x02 This command is very technical, and only intended for debugging. Misuse can cause the bridge to be disconnected!\x02
\x02UUQUERY\x0f \x1duuid\x0f
    Returns the nickname associated with UUID \x1duuid\x0f, if there is one.
\x02NICKQUERY\x0f \x1dnick\x0f
    Returns the UUID for the nickname \x1dnick\x0f, if we know it.
\x02*** End of subcommand help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        extract_command!(inp, cmd, rest);
        match (cmd, rest) {
            ("raw", args) => Some(InspCommand::Raw(args[1..].join(" "))),
            ("uuquery", &[uuid]) => Some(InspCommand::QueryUuid(uuid.to_owned())),
            ("nickquery", &[uuid]) => Some(InspCommand::QueryNickUuid(uuid.to_owned())),
            _ => None
        }
    }
}
pub enum AdminCommand {
    Ghost(String, GhostCommand),
    Contact(ContactCommand),
    Whatsapp(WhatsappCommand),
    Modem(ModemCommand),
    Group(GroupCommand),
    Insp(InspCommand),
    Help(Option<String>)
}
impl AdminCommand {
    pub fn help() -> &'static str {
       "\x02*** sms-irc help ***\x0f
The following commands are available. Use \x02HELP\x02 \x1dcommand\x0f to get more information on each subcommand.
\x02GHOST\x02 \x1dnickname subcommand\x0f
    Manages ghosts (i.e. IRC users that represent a message recipient).
    Here, \x1dnickname\x0f represents the nickname of the ghost you're trying to manage.
    \x1dFor example, \x11GHOST user REMOVE\x11 would remove the ghost named \x11user\x11.\x1d
\x02CONTACT\x0f \x1dsubcommand\x0f
    Allows establishing contact with new people.
\x02WHATSAPP\x0f \x1dsubcommand\x0f
    Manages WhatsApp registration, login and more.
\x02MODEM\x0f \x1dsubcommand\x0f
    Queries the modem state and provides other management commands.
\x02GROUP\x0f \x1dsubcommand\x0f
    Used to bridge and unbridge group chats to/from IRC channels.
    (Currently, only WhatsApp group chats are supported.)
\x02INSP\x0f \x1dsubcommand\x0f
    If sms-irc is using the InspIRCd server-to-server link, provides some debug commands.
\x02HELP\x0f \x1d[command]\x0f
    Shows this help.
    If a \x1dcommand\x0f is provided, shows help for that command.
\x02*** End of help ***\x0f"
    }
    pub fn parse(inp: &[&str]) -> Option<Self> {
        if inp.len() < 1 {
            return None;
        }
        match &inp[0].to_lowercase() as &str {
            "contact" => {
                ContactCommand::parse(&inp[1..])
                    .map(|x| AdminCommand::Contact(x))
            },
            "whatsapp" => {
                WhatsappCommand::parse(&inp[1..])
                    .map(|x| AdminCommand::Whatsapp(x))
            },
            "group" => {
                GroupCommand::parse(&inp[1..])
                    .map(|x| AdminCommand::Group(x))
            },
            "modem" => {
                ModemCommand::parse(&inp[1..])
                    .map(|x| AdminCommand::Modem(x))
            },
            "insp" => {
                InspCommand::parse(&inp[1..])
                    .map(|x| AdminCommand::Insp(x))
            },
            "ghost" => {
                if inp.len() < 3 {
                    return None;
                }
                let tgt = inp[1].to_owned();
                GhostCommand::parse(&inp[2..])
                    .map(|x| AdminCommand::Ghost(tgt, x))
            },
            "help" => {
                let page = inp.get(1).map(|x| x.to_string());
                Some(AdminCommand::Help(page))
            }
            _ => None
        }
    }
}
pub fn help_page(query: Option<&str>) -> Option<&'static str> {
    let query = query.map(|x| x.to_lowercase());
    match query.as_ref().map(|x| x as &str) {
        Some("ghost") => Some(GhostCommand::help()),
        Some("modem") => Some(ModemCommand::help()),
        Some("contact") => Some(ContactCommand::help()),
        Some("whatsapp") => Some(WhatsappCommand::help()),
        Some("group") => Some(GroupCommand::help()),
        Some("insp") => Some(InspCommand::help()),
        None => Some(AdminCommand::help()),
        _ => None
    }
}
