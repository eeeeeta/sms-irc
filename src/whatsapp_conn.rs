//! Connecting to WhatsApp Web, and handling the intermediate states when doing so.
//! 
//! FIXME: maybe find a way to share code between this and `ModemInner` from
//! `src/modem.rs`.

use whatsappweb::conn::WebConnection;
use whatsappweb::event::WaEvent;
use whatsappweb::req::WaRequest;
use whatsappweb::errors::WaError;
use whatsappweb::session::PersistentSession;
use tokio_timer::Delay;
use std::time::{Duration, Instant};
use failure::Error;
use futures::{self, Future, Stream, Poll, Async, Sink, StartSend};

pub struct WebConnectionWrapperConfig {
    pub backoff_time_ms: u64
}

enum WrapperState {
    Disabled,
    Waiting(Delay),
    Initializing(Box<dyn Future<Item = WebConnection, Error = WaError>>),
    Running(WebConnection)
}
pub struct WebConnectionWrapper {
    inner: WrapperState,
    persist: Option<PersistentSession>,
    cfg: WebConnectionWrapperConfig
}

impl WebConnectionWrapper {
    pub fn new(cfg: WebConnectionWrapperConfig) -> Self {
        Self {
            inner: WrapperState::Disabled,
            persist: None,
            cfg
        }
    }
    pub fn disable(&mut self) {
        self.inner = WrapperState::Disabled;
    }
    pub fn connect_new(&mut self) {
        self.set_persistent(None);
        self.initialize();
    }
    pub fn connect_persistent(&mut self, persist: PersistentSession) {
        self.set_persistent(Some(persist));
        self.initialize();
    }
    pub fn set_persistent(&mut self, persist: Option<PersistentSession>) {
        self.persist = persist;
    }
    pub fn is_connected(&self) -> bool {
        if let WrapperState::Running(_) = self.inner {
            true
        }
        else {
            false
        }
    }
    fn initialize(&mut self) {
        info!("Connecting to WhatsApp Web...");
        let fut: Box<dyn Future<Item = WebConnection, Error = WaError>> = match self.persist.clone() {
            Some(p) => Box::new(WebConnection::connect_persistent(p)),
            None => Box::new(WebConnection::connect_new())
        };
        self.inner = WrapperState::Initializing(fut);
    }
    fn backoff(&mut self) {
        let delay = Delay::new(Instant::now() + Duration::from_millis(self.cfg.backoff_time_ms));
        self.inner = WrapperState::Waiting(delay);
    }
}
impl Stream for WebConnectionWrapper {
    type Item = Result<WaEvent, WaError>;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Result<WaEvent, WaError>>, Error> {
        use self::WrapperState::*;
        loop {
            match std::mem::replace(&mut self.inner, Disabled) {
                Disabled => return Ok(Async::NotReady),
                Waiting(mut delay) => {
                    match delay.poll()? {
                        Async::Ready(_) => self.initialize(),
                        Async::NotReady => {
                            self.inner = Waiting(delay);
                            return Ok(Async::NotReady);
                        },
                    }
                },
                Initializing(mut fut) => {
                    match fut.poll() {
                        Ok(Async::Ready(c)) => {
                            info!("Connected to WhatsApp Web.");
                            self.inner = Running(c);
                        },
                        Ok(Async::NotReady) => {
                            self.inner = Initializing(fut);
                            return Ok(Async::NotReady);
                        },
                        Err(e) => {
                            warn!("Failed to connect to WA: {}", e);
                            self.backoff();
                        }
                    }
                },
                Running(mut fut) => {
                    match fut.poll() {
                        Ok(Async::NotReady) => {
                            self.inner = Running(fut);
                            return Ok(Async::NotReady);
                        },
                        Ok(Async::Ready(Some(x))) => {
                            self.inner = Running(fut);
                            return Ok(Async::Ready(Some(Ok(x))));
                        },
                        Ok(Async::Ready(None)) => {
                            unreachable!()
                        },
                        Err(e) => {
                            self.backoff();
                            return Ok(Async::Ready(Some(Err(e))));
                        }
                    }
                },
            }
        }
    }
}
impl Sink for WebConnectionWrapper {
    type SinkItem = WaRequest;
    type SinkError = WaError;

    fn start_send(&mut self, item: WaRequest) -> StartSend<WaRequest, WaError> {
        if let WrapperState::Running(ref mut c) = self.inner {
            match c.start_send(item) {
                Err(e) => {
                    warn!("WA sink failed: {}", e);
                    self.backoff();
                    return Err(e);
                },
                x => x
            }
        }
        else {
            panic!("WebConnectionWrapper should not be used as a sink while disconnected");
        }
    }
    fn poll_complete(&mut self) -> Poll<(), WaError> {
        if let WrapperState::Running(ref mut c) = self.inner {
            c.poll_complete()
        }
        else {
            panic!("WebConnectionWrapper should not be used as a sink while disconnected");
        }
    }
}
