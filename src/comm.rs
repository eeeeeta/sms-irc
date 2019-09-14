//! Communication between different things.

use futures::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use huawei_modem::cmd::sms::SmsMessage;
use huawei_modem::pdu::PduAddress;
use whatsappweb::Jid;
use crate::config::Config;
use crate::store::Store;
use tokio_core::reactor::Handle;
use crate::whatsapp_media::MediaResult;

pub enum ModemCommand {
    DoCmgl,
    CmglComplete(Vec<SmsMessage>),
    CmglFailed(failure::Error),
    SendMessage(PduAddress, String),
    RequestCsq,
    RequestReg,
    ForceReinit,
    UpdatePath(Option<String>),
    CommandTimeout,
    MakeContact(PduAddress),
}
pub enum WhatsappCommand {
    StartRegistration,
    LogonIfSaved,
    SendGroupMessage(String, String),
    SendDirectMessage(PduAddress, String),
    GroupAssociate(Jid, String),
    GroupList,
    GroupRemove(String),
    GroupUpdateAll,
    MediaFinished(MediaResult),
    PrintAcks,
    MakeContact(PduAddress),
    SubscribePresence(PduAddress)
}
#[allow(dead_code)]
pub enum ContactFactoryCommand {
    ProcessMessages,
    ProcessGroups,
    ProcessAvatars,
    SetupContact(PduAddress),
    DropContact(PduAddress),
    QueryContact(PduAddress, i32),
    // FIXME: these `ByNick` variants are dumb and only exist to serve the control bot
    DropContactByNick(String),
    LoadRecipients,
    ForwardCommand(PduAddress, ContactManagerCommand),
    ForwardCommandByNick(String, ContactManagerCommand),
    SubscribePresenceByNick(String)
}
pub enum ContactManagerCommand {
    ProcessMessages,
    ProcessGroups,
    UpdateAway(Option<String>),
    ChangeNick(String, i32),
    SetWhatsapp(bool)
}
#[derive(Clone)]
pub enum ControlBotCommand {
    Log(String),
    ReportFailure(String),
    CommandResponse(String),
    ProcessGroups
}
pub struct InitParameters<'a, T: 'a> {
    pub cfg: &'a Config,
    pub cfg2: &'a T,
    pub store: Store,
    pub cm: &'a mut ChannelMaker,
    pub hdl: &'a Handle
}
pub struct ChannelMaker {
    pub modem_rx: Option<UnboundedReceiver<ModemCommand>>,
    pub modem_tx: UnboundedSender<ModemCommand>,
    pub cf_rx: Option<UnboundedReceiver<ContactFactoryCommand>>,
    pub cf_tx: UnboundedSender<ContactFactoryCommand>,
    pub cb_rx: Option<UnboundedReceiver<ControlBotCommand>>,
    pub cb_tx: UnboundedSender<ControlBotCommand>,
    pub wa_rx: Option<UnboundedReceiver<WhatsappCommand>>,
    pub wa_tx: UnboundedSender<WhatsappCommand>
}
impl ChannelMaker {
    pub fn new() -> Self {
        let (modem_tx, modem_rx) = mpsc::unbounded();
        let (cf_tx, cf_rx) = mpsc::unbounded();
        let (cb_tx, cb_rx) = mpsc::unbounded();
        let (wa_tx, wa_rx) = mpsc::unbounded();
        Self {
            modem_rx: Some(modem_rx),
            modem_tx,
            cf_rx: Some(cf_rx),
            cf_tx,
            cb_rx: Some(cb_rx),
            cb_tx,
            wa_rx: Some(wa_rx),
            wa_tx
        }
    }
}

