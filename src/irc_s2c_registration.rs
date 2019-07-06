//! Handling IRC s2c connection registration.

use futures::{Future, Async, Poll, Stream, Sink, self};
use irc::proto::IrcCodec;
use tokio_core::net::TcpStream;
use tokio_codec::Framed;
use irc::proto::message::Message;
use irc::proto::command::Command;
use failure::Error;
use std::net::SocketAddr;

use crate::util::Result;
use crate::irc_s2c_v3::{IrcCap, SUPPORTED_CAPS};
use crate::irc_s2c::{IrcConnection, SERVER_NAME};

pub struct RegistrationInformation {
    pub nick: String,
    pub user: String,
    pub realname: String,
    pub caps: Vec<IrcCap>
}
struct PendingIrcConnection {
    sock: Framed<TcpStream, IrcCodec>,
    addr: SocketAddr,
    cap_ended: bool,
    nick: Option<String>,
    user: Option<String>,
    realname: Option<String>,
    caps: Vec<IrcCap>,
    outbox: Vec<Message>,
    new: bool
}
pub struct PendingIrcConnectionWrapper {
    inner: Option<PendingIrcConnection>
}
impl PendingIrcConnectionWrapper {
    pub fn from_incoming(ts: TcpStream, sa: SocketAddr) -> Result<Self> {
        let codec = IrcCodec::new("utf8")?;
        let ic = PendingIrcConnection {
            sock: Framed::new(ts, codec),
            addr: sa,
            cap_ended: true,
            nick: None,
            user: None,
            realname: None,
            caps: vec![],
            outbox: vec![],
            new: true
        };
        Ok(Self { inner: Some(ic) })
    }
}

impl Future for PendingIrcConnectionWrapper {
    type Item = IrcConnection;
    type Error = Error;

    fn poll(&mut self) -> Poll<IrcConnection, Error> {
        let conn = self.inner.as_mut()
            .expect("PendingIrcConnectionWrapper polled after completion");
        if conn.new {
            conn.new = false;
            conn.on_new()?;
        }
        while let Async::Ready(msg) = conn.sock.poll()? {
            let msg = msg.ok_or(format_err!("Socket disconnected"))?;
            trace!("<-- [{}] {}", conn.addr, msg);
            conn.handle_remote_message(msg)?;
        }
        sink_outbox!(conn, outbox, sock, conn.addr);
        // we can do this sorta stuff now, because NLL
        if conn.is_done()? {
            let conn = self.inner.take().unwrap();
            let reginfo = RegistrationInformation {
                nick: conn.nick.unwrap(),
                user: conn.user.unwrap(),
                realname: conn.realname.unwrap(),
                caps: conn.caps
            };
            let ret = IrcConnection::from_pending(conn.sock, conn.addr, reginfo);
            Ok(Async::Ready(ret))
        }
        else {
            Ok(Async::NotReady)
        }
    }
}
impl PendingIrcConnection {
    fn reply(&mut self, cmd: &str, mut args: Vec<&str>, suffix: Option<&str>) -> Result<()> {
        args.insert(0, "*");
        self.outbox.push(Message::new(Some(&SERVER_NAME), cmd, args, suffix)?);
        Ok(())
    }
    fn on_new(&mut self) -> Result<()> {
        self.reply("NOTICE", vec![], Some("sms-irc initialized, please go on"))?;
        Ok(())
    }
    fn handle_remote_message(&mut self, msg: Message) -> Result<()> {
        use irc::proto::command::CapSubCommand;

        match msg.command {
            Command::NICK(name) => {
                // FIXME: add some sort of validation?
                self.nick = Some(name);
            },
            Command::USER(user, _, realname) => {
                self.user = Some(user);
                self.realname = Some(realname);
            },
            Command::CAP(_, CapSubCommand::LS, _, _) => {
                // Setting cap_ended here means is_done() won't
                // return true until the user sends CAP END.
                self.cap_ended = false;
                self.reply("CAP", vec!["LS"], Some(SUPPORTED_CAPS))?;
            },
            Command::CAP(_, CapSubCommand::REQ, Some(caps), None) | Command::CAP(_, CapSubCommand::REQ, None, Some(caps)) => {
                for cap in caps.split(" ") {
                    if let Some(c) = IrcCap::from_cap_name(cap) {
                        debug!("Negotiated capability: {:?}", c);
                        self.caps.push(c);
                        self.reply("CAP", vec!["ACK"], Some(cap))?;
                    }
                    else {
                        self.reply("CAP", vec!["NAK"], Some(cap))?;
                    }
                }
            },
            Command::CAP(_, CapSubCommand::LIST, _, _) => {
                // FIXME: this iterator chain may not be optimal
                let caps = self.caps
                    .iter()
                    .map(|x| x.cap_name())
                    .collect::<Vec<_>>()
                    .join(" ");
                self.reply("CAP", vec!["LIST"], Some(&caps))?;
            },
            Command::CAP(_, CapSubCommand::END, _, _) => {
                self.cap_ended = true;
            },
            Command::PASS(_) => {
                // FIXME: maybe add support in the future?
            },
            Command::QUIT(_) => {
                Err(format_err!("Client quit"))?
            },
            Command::Raw(cmd, args, trailing) => {
                if cmd == "USER" && args.len() > 1 {
                    if trailing.is_none() {
                        self.user = Some(args[0].to_owned());
                        self.realname = Some(args.last().unwrap().to_owned());
                    }
                    else {
                        self.user = Some(args[0].to_owned());
                        self.realname = trailing;
                    }
                }
                else {
                    warn!("Unexpected raw registration command: {} {:?} {:?}", cmd, args, trailing);
                    self.reply("451", vec![], Some("You have not registered"))?;
                }
            },
            c => {
                let st: String = (&c).into();
                warn!("Unexpected registration command: {}", st.trim());
                self.reply("451", vec![], Some("You have not registered"))?;
            }
        }
        Ok(())
    }
    fn is_done(&mut self) -> Result<bool> {
        let ret = self.cap_ended
            && self.nick.is_some()
            && self.user.is_some()
            && self.realname.is_some()
            && self.sock.poll_complete()? == Async::Ready(());
        Ok(ret)
    }
}
