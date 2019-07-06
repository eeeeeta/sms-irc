//! Acting as an IRC server (IRC server-to-client protocol)
//!
//! Based heavily off https://modern.ircdocs.horse/

use tokio_core::net::{TcpListener, Incoming, TcpStream};
use tokio_codec::Framed;
use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use irc::proto::IrcCodec;
use irc::proto::message::Message;
use irc::proto::command::Command;
use futures::{Future, Async, Poll, Stream, Sink, AsyncSink, self};
use failure::Error;
use std::net::{SocketAddr, ToSocketAddrs};

use crate::util::{self, Result};
use crate::irc_s2c_registration::{PendingIrcConnectionWrapper, RegistrationInformation};
use crate::config::IrcServerConfig;
use crate::comm::InitParameters;
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
    new: bool

}

pub struct IrcServer {
    cfg: IrcServerConfig,
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
            let pending = PendingIrcConnectionWrapper::from_incoming(ts, sa)?;
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
        for (i, c) in self.connections.iter_mut().enumerate() {
            if let Err(e) = c.poll() {
                info!("Connection on {} closed: {}", c.addr, e);
                to_remove.push(i);
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
            store, cfg, incoming,
            connections: vec![],
            pending: vec![]
        })
    }
}

impl IrcConnection {
    pub fn from_pending(
        sock: Framed<TcpStream, IrcCodec>,
        addr: SocketAddr,
        reginfo: RegistrationInformation
        ) -> Self {
        Self {
            sock, addr, reginfo,
            outbox: vec![],
            new: true
        }
    }
    fn reply_s2c<'a, T: Into<Option<&'a str>>>(&mut self, cmd: &str, args: Vec<&str>, suffix: T) -> Result<()> {
        let mut new_args = vec![&self.reginfo.nick as &str];
        new_args.extend(args.into_iter());
        self.outbox.push(Message::new(Some(&SERVER_NAME), cmd, new_args, suffix.into())?);
        Ok(())
    }
    fn on_new(&mut self) -> Result<()> {
        self.reply_s2c("001", vec![], "Welcome to sms-irc, a SMS/WhatsApp to IRC bridge!")?;
        self.reply_s2c("002", vec![], &format!("This is sms-irc version {}, running in IRC server mode.", env!("CARGO_PKG_VERSION")) as &str)?;
        self.reply_s2c("003", vec![], "(This server doesn't keep creation timestamp information at present.)")?;
        let server_version = format!("sms-irc-{}", env!("CARGO_PKG_VERSION"));
        self.reply_s2c("004", vec![SERVER_NAME, &server_version, USER_MODES, CHANNEL_MODES], None)?;
        self.reply_s2c("005",
                       vec!["AWAYLEN=200", "CASEMAPPING=ascii", "NETWORK=sms-irc", "NICKLEN=100"],
                       "are supported by this server")?;
        self.send_motd()?;
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
