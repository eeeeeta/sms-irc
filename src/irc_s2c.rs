//! Acting as an IRC server (IRC server-to-client protocol)
//!
//! Based heavily off https://modern.ircdocs.horse/

use tokio_core::net::{TcpListener, Incoming, TcpStream};
use tokio_codec::Framed;
use irc::proto::IrcCodec;
use irc::proto::message::Message;
use irc::proto::command::Command;
use futures::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use futures::{Future, Async, Poll, Stream, Sink, self};
use failure::{Error, format_err};
use std::net::{SocketAddr, ToSocketAddrs};
use std::collections::VecDeque;
use huawei_modem::pdu::DeliverPdu;

use crate::util::{Result, self};
use crate::sender_common::Sender;
use crate::irc_s2c_registration::{PendingIrcConnectionWrapper, RegistrationInformation};
use crate::config::IrcServerConfig;
use crate::comm::InitParameters;
use crate::models::Group;
use crate::comm::*;
use crate::store::Store;

pub static SERVER_NAME: &str = "sms-irc.";
pub static USER_MODES: &str = "i";
pub static CHANNEL_MODES: &str = "nt";
pub static MOTD: &str = r#"Welcome to sms-irc!

This is the experimental IRC server backend.
Please refer to https://git.theta.eu.org/sms-irc.git/about/
for more information about sms-irc.

Alternatively, come and chat to us in #sms-irc on chat.freenode.net
if you have comments or want help using the software!"#;

pub struct IrcConnection {
    sock: Framed<TcpStream, IrcCodec>,
    addr: SocketAddr,
    reginfo: RegistrationInformation,
    outbox: Vec<Message>,
    store: Store,
    joined_groups: Vec<Group>,
    wa_outbox: VecDeque<WhatsappCommand>,
    m_outbox: VecDeque<ModemCommand>,
    new: bool
}

pub struct IrcServer {
    cf_rx: UnboundedReceiver<ContactFactoryCommand>,
    cb_rx: UnboundedReceiver<ControlBotCommand>,
    wa_tx: UnboundedSender<WhatsappCommand>,
    m_tx: UnboundedSender<ModemCommand>,
    _cfg: IrcServerConfig,
    store: Store,
    incoming: Incoming,
    connections: Vec<IrcConnection>,
    pending: Vec<PendingIrcConnectionWrapper>
}

impl Future for IrcConnection {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> { 
        if self.new {
            self.new = false;
            self.on_new()?;
        }
        while let Async::Ready(msg) = self.sock.poll()? {
            let msg = msg.ok_or(format_err!("Socket disconnected"))?;
            trace!("<-- [{}] {}", self.addr, msg);
            self.handle_remote_message(msg)?;
        }
        sink_outbox!(self, outbox, sock, self.addr);
        Ok(Async::NotReady)
    }
}

impl Future for IrcServer {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Error> {
        while let Async::Ready(inc) = self.incoming.poll()? {
            let (ts, sa) = inc.ok_or(format_err!("TCP listener stopped"))?;
            info!("New connection from {}", sa);
            let pending = PendingIrcConnectionWrapper::from_incoming(ts, sa, self.store.clone())?;
            self.pending.push(pending);
        }
        let mut to_remove = vec![];
        for (i, p) in self.pending.iter_mut().enumerate() {
            match p.poll() {
                Ok(Async::Ready(c)) => {
                    info!("Connection on {} completed registration", c.addr);
                    self.connections.push(c);
                    to_remove.push(i);
                },
                Ok(Async::NotReady) => {},
                Err(e) => {
                    to_remove.push(i);
                    info!("Pending connection closed: {}", e);
                }
            }
        }
        while let Some(i) = to_remove.pop() {
            self.pending.remove(i);
        }
        while let Async::Ready(cbc) = self.cb_rx.poll().unwrap() {
            let cbc = cbc.ok_or(format_err!("cb_rx stopped"))?;
            self.handle_control(cbc)?;
        }
        while let Async::Ready(cfc) = self.cf_rx.poll().unwrap() {
            let cfc = cfc.ok_or(format_err!("cf_rx stopped"))?;
            self.handle_contact(cfc)?;
        }
        for (i, c) in self.connections.iter_mut().enumerate() {
            if let Err(e) = c.poll() {
                info!("Connection on {} closed: {}", c.addr, e);
                to_remove.push(i);
            }
            while let Some(wac) = c.wa_outbox.pop_front() {
                self.wa_tx.unbounded_send(wac).unwrap();
            }
            while let Some(mc) = c.m_outbox.pop_front() {
                self.m_tx.unbounded_send(mc).unwrap();
            }
        }
        while let Some(i) = to_remove.pop() {
            self.connections.remove(i);
        }
        Ok(Async::NotReady)
    }
}

impl IrcServer {
    pub fn new(p: InitParameters<IrcServerConfig>) -> Result<Self> {
        let store = p.store;
        let cfg = p.cfg2.clone();
        let addr = cfg.listen.to_socket_addrs()?
            .nth(0)
            .ok_or(format_err!("no listen addresses found"))?;
        let listener = TcpListener::bind(&addr, &p.hdl)?;
        info!("Listening on {} for connections", addr);
        let incoming = listener.incoming();
        Ok(Self {
            store, _cfg: cfg, incoming,
            cb_rx: p.cm.cb_rx.take().unwrap(),
            cf_rx: p.cm.cf_rx.take().unwrap(),
            wa_tx: p.cm.wa_tx.clone(),
            m_tx: p.cm.modem_tx.clone(),
            connections: vec![],
            pending: vec![]
        })
    }
    pub fn handle_control(&mut self, cmd: ControlBotCommand) -> Result<()> {
        for c in self.connections.iter_mut() {
            if let Err(e) = c.handle_control(cmd.clone()) {
                warn!("Connection on {} failed to handle control: {}", c.addr, e);
            }
        }
        Ok(())
    }
    pub fn handle_contact(&mut self, cfc: ContactFactoryCommand) -> Result<()> {
        if let ContactFactoryCommand::ProcessMessages = cfc {
            for c in self.connections.iter_mut() {
                if let Err(e) = c.process_messages() {
                    warn!("Connection on {} failed to process messages: {}", c.addr, e);
                }
            }
        }
        Ok(())
    }
}

impl IrcConnection {
    pub fn from_pending(
        sock: Framed<TcpStream, IrcCodec>,
        addr: SocketAddr,
        store: Store,
        reginfo: RegistrationInformation
        ) -> Self {
        Self {
            sock, addr, reginfo, store,
            outbox: vec![],
            joined_groups: vec![],
            wa_outbox: VecDeque::new(),
            m_outbox: VecDeque::new(),
            new: true
        }
    }
    pub fn handle_control(&mut self, cmd: ControlBotCommand) -> Result<()> {
        use self::ControlBotCommand::*;
        match cmd {
            Log(thing) => {
                self.outbox.push(Message::new(Some("root"), "PRIVMSG", vec!["&smsirc"], Some(&thing))?);
            },
            ReportFailure(thing) => {
                // FIXME: make shoutier
                self.reply_s2c("PRIVMSG", vec![], Some(&thing as &str))?;
            },
            CommandResponse(thing) => {
                self.reply_s2c("NOTICE", vec![], Some(&thing as &str))?;
            },
            ProcessGroups => {}
        }
        Ok(())
    }
    fn reply_s2c<'a, T: Into<Option<&'a str>>>(&mut self, cmd: &str, args: Vec<&str>, suffix: T) -> Result<()> {
        let mut new_args = vec![&self.reginfo.nick as &str];
        new_args.extend(args.into_iter());
        self.outbox.push(Message::new(Some(&SERVER_NAME), cmd, new_args, suffix.into())?);
        Ok(())
    }
    fn reply_from_user<'a, T: Into<Option<&'a str>>>(&mut self, cmd: &str, args: Vec<&str>, suffix: T) -> Result<()> {
        let host = format!("{}!{}@{}", self.reginfo.nick, self.reginfo.user, self.addr.ip());
        self.outbox.push(Message::new(Some(&host), cmd, args, suffix.into())?);
        Ok(())
    }
    fn reply_from_nick<'a, T: Into<Option<&'a str>>>(&mut self, from: &str, cmd: &str, args: Vec<&str>, suffix: T) -> Result<()> {
        // FIXME: make hostmask not suck
        let host = format!("{}!{}@sms-irc.", from, from);
        self.outbox.push(Message::new(Some(&host), cmd, args, suffix.into())?);
        Ok(())
    }
    fn on_new(&mut self) -> Result<()> {
        self.reply_s2c("001", vec![], "Welcome to sms-irc, a SMS/WhatsApp to IRC bridge!")?;
        self.reply_s2c("002", vec![], &format!("This is sms-irc version {}, running in IRC server mode.", env!("CARGO_PKG_VERSION")) as &str)?;
        self.reply_s2c("003", vec![], "(This server doesn't keep creation timestamp information at present.)")?;
        let server_version = format!("sms-irc-{}", env!("CARGO_PKG_VERSION"));
        self.reply_s2c("004", vec![SERVER_NAME, &server_version, USER_MODES, CHANNEL_MODES], None)?;
        self.reply_s2c("005",
                       vec!["AWAYLEN=200", "CASEMAPPING=ascii", "NETWORK=sms-irc", "NICKLEN=100", "PREFIX=(qaohv)~&@%+"],
                       "are supported by this server")?;
        self.send_motd()?;
        self.setup_control_channel()?;
        for grp in self.store.get_all_groups()? {
            self.setup_group(grp)?;
        }
        Ok(())
    }
    pub fn process_messages(&mut self) -> Result<()> {
        use std::convert::TryFrom;

        for msg in self.store.get_all_messages()? {
            debug!("Processing message #{}", msg.id);
            let addr = util::un_normalize_address(&msg.phone_number)
                .ok_or(format_err!("invalid address {} in db", msg.phone_number))?;
            let recip = match self.store.get_recipient_by_addr_opt(&addr)? {
                Some(r) => r,
                None => {
                    warn!("stub impl doesn't make new recipients yet");
                    continue;
                },
            };
            if msg.pdu.is_some() {
                let pdu = DeliverPdu::try_from(msg.pdu.as_ref().unwrap() as &[u8])?;
                self.process_msg_pdu(&recip.nick, msg, pdu)?;
            }
            else {
                self.process_msg_plain(&recip.nick, msg)?;
            }
        }
        Ok(())
    }
    fn setup_group(&mut self, grp: Group) -> Result<()> {
        self.reply_from_user("JOIN", vec![&grp.channel], None)?;
        self.reply_s2c("332", vec![&grp.channel], Some(&grp.topic as &str))?;
        self.reply_s2c("353", vec!["=", &grp.channel], Some(&format!("&{}", self.reginfo.nick) as &str))?;
        let mut recips = Vec::with_capacity(grp.participants.len());
        for id in grp.participants.iter() {
            recips.push(self.store.get_recipient_by_id_opt(*id)?
                        .ok_or(format_err!("recipient group wat"))?);
        }
        for recips in recips.chunks(5) {
            let nicks = recips
                .iter()
                .map(|x| {
                    let op = if grp.admins.contains(&x.id) {
                        "@"
                    }
                    else {
                        ""
                    };
                    format!("{}{} ", op, x.nick)
                })
                .collect::<String>();
            self.reply_s2c("353", vec!["@", &grp.channel], Some(&nicks as &str))?;
        }
        self.reply_s2c("366", vec![&grp.channel], Some("End of /NAMES list."))?;
        self.reply_s2c("324", vec![&grp.channel, "+nt"], None)?;
        self.joined_groups.push(grp);
        Ok(())
    }
    fn setup_control_channel(&mut self) -> Result<()> {
        self.reply_from_user("JOIN", vec!["&smsirc"], None)?;
        self.reply_s2c("332", vec!["&smsirc"], Some("sms-irc admin channel"))?;
        self.reply_s2c("353", vec!["@", "&smsirc"], Some(&format!("&{} ~root", self.reginfo.nick) as &str))?;
        for recips in self.store.get_all_recipients()?.chunks(5) {
            let nicks = recips
                .iter()
                .map(|x| format!("{} ", x.nick))
                .collect::<String>();
            self.reply_s2c("353", vec!["@", "&smsirc"], Some(&nicks as &str))?;
        }
        self.reply_s2c("366", vec!["&smsirc"], Some("End of /NAMES list."))?;
        self.reply_s2c("324", vec!["&smsirc"], None)?;
        Ok(())
    }
    fn send_motd(&mut self) -> Result<()> {
        self.reply_s2c("375", vec![], "- Message of the day -")?;
        for line in MOTD.lines() {
            self.reply_s2c("372", vec![], Some(line))?;
        }
        self.reply_s2c("376", vec![], "End of /MOTD command.")?;
        Ok(())
    }
    fn handle_remote_message(&mut self, msg: Message) -> Result<()> {
        match msg.command {
            Command::PING(tok, _) => {
                self.reply_s2c("PONG", vec![], Some(&tok as &str))?;
            },
            Command::QUIT(_) => {
                Err(format_err!("Client quit"))?;
            },
            Command::NICK(new) => {
                self.reply_from_user("NICK", vec![&new], None)?;
                self.reginfo.nick = new;
            },
            Command::JOIN(chan, _, _) => {
                self.reply_s2c("405", vec![&chan], Some("You may not manually /JOIN channels in this alpha version."))?;
            },
            Command::PART(chan, _) => {
                // This is ERR_NOTONCHANNEL, which isn't amazing.
                self.reply_s2c("442", vec![&chan], Some("You may not manually /PART channels in this alpha version."))?;
            },
            Command::PRIVMSG(target, msg) => {
                if target.starts_with("#") {
                    // FIXME: check the channel actually exists
                    self.wa_outbox.push_back(WhatsappCommand::SendGroupMessage(target, msg));
                }
                else {
                    if let Some(recip) = self.store.get_recipient_by_nick_opt(&target)? { 
                        let addr = util::un_normalize_address(&recip.phone_number)
                            .ok_or(format_err!("unnormalizable addr"))?;
                        if recip.whatsapp {
                            self.wa_outbox.push_back(WhatsappCommand::SendDirectMessage(addr, msg));
                        }
                        else {
                            self.m_outbox.push_back(ModemCommand::SendMessage(addr, msg));
                        }
                    }
                    else {
                        self.reply_s2c("401", vec![&target], "Unknown nickname.")?;
                    }
                }
            },
            u => {
                // FIXME: the irc crate is hacky, and requires hacky workarounds
                let st: String = (&u).into();
                warn!("Got unknown command: {}", st.trim());
                let verb = st.split(" ").next().unwrap();
                self.reply_s2c("421", vec![verb], "Unknown or unimplemented command.")?;
            }
        }
        Ok(())
    }
}

impl Sender for IrcConnection {
    fn report_error(&mut self, from_nick: &str, err: String) -> Result<()> {
        self.reply_from_nick(from_nick, "NOTICE", vec![&self.reginfo.nick.clone()], Some(&err as &str))?;
        Ok(())
    }
    fn store(&mut self) -> &mut Store {
        &mut self.store
    }
    fn private_target(&mut self) -> String {
        self.reginfo.nick.clone()
    }
    fn send_irc_message(&mut self, from_nick: &str, to: &str, msg: &str) -> Result<()> {
        self.reply_from_nick(from_nick, "PRIVMSG", vec![to], Some(&msg as &str))?;
        Ok(())
    }
}
