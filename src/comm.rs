//! Communication between different things.

use futures::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use huawei_modem::cmd::sms::SmsMessage;
use huawei_modem::errors::HuaweiError;
use huawei_modem::pdu::PduAddress;
use huawei_modem::cmd::network::{SignalQuality, RegistrationState};
use config::Config;
use store::Store;
use tokio_core::reactor::Handle;

pub enum ModemCommand {
    DoCmgl,
    CmglComplete(Vec<SmsMessage>),
    CmglFailed(HuaweiError),
    SendMessage(PduAddress, String),
    RequestCsq,
    RequestReg
}
pub enum ContactFactoryCommand {
    ProcessMessages,
    MakeContact(PduAddress),
    DropContact(PduAddress),
    LoadRecipients
}
pub enum ContactManagerCommand {
    ProcessMessages
}
pub enum ControlBotCommand {
    Log(String),
    CsqResult(SignalQuality),
    RegResult(RegistrationState)
}
pub struct InitParameters<'a> {
    pub cfg: &'a Config,
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
    pub cb_tx: UnboundedSender<ControlBotCommand>
}
impl ChannelMaker {
    pub fn new() -> Self {
        let (modem_tx, modem_rx) = mpsc::unbounded();
        let (cf_tx, cf_rx) = mpsc::unbounded();
        let (cb_tx, cb_rx) = mpsc::unbounded();
        Self {
            modem_rx: Some(modem_rx),
            modem_tx,
            cf_rx: Some(cf_rx),
            cf_tx,
            cb_rx: Some(cb_rx),
            cb_tx
        }
    }
}

