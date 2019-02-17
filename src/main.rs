#![allow(proc_macro_derive_resolution_fallback)]

extern crate irc;
extern crate futures;
extern crate tokio_core;
extern crate huawei_modem;
#[macro_use] extern crate diesel;
extern crate r2d2;
extern crate r2d2_diesel;
extern crate serde;
#[macro_use] extern crate serde_derive;
extern crate toml;
#[macro_use] extern crate failure;
#[macro_use] extern crate log;
extern crate log4rs;
extern crate tokio_timer;
extern crate whatsappweb;
extern crate serde_json;
extern crate image;
extern crate qrcode;
extern crate tokio_codec;
extern crate chrono;
extern crate humansize;
extern crate uuid;
extern crate reqwest;
extern crate mime_guess;
extern crate tokio_signal;
extern crate regex;
#[macro_use] extern crate lazy_static;

mod config;
mod store;
mod modem;
mod comm;
mod util;
mod schema;
mod models;
mod contact;
mod contact_factory;
mod contact_common;
mod control;
mod control_common;
mod sender_common;
mod whatsapp;
mod whatsapp_media;
mod insp_s2s;
mod insp_user;

use config::Config;
use store::Store;
use modem::ModemManager;
use control::ControlBot;
use comm::{ChannelMaker, InitParameters};
use futures::{Future, Stream};
use contact_factory::ContactFactory;
use futures::sync::mpsc::UnboundedSender;
use comm::ControlBotCommand;
use tokio_core::reactor::Core;
use log4rs::config::{Appender, Logger, Root};
use log4rs::config::Config as LogConfig;
use log4rs::append::Append;
use log4rs::append::console::ConsoleAppender;
use log::Record;
use std::fmt;
use insp_s2s::InspLink;
use whatsapp::WhatsappManager;
use tokio_signal::unix::{Signal, SIGHUP};

pub struct IrcLogWriter {
    sender: UnboundedSender<ControlBotCommand>,
    level: log::LevelFilter
}
impl fmt::Debug for IrcLogWriter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IrcLogWriter {{ /* fields hidden */ }}")
    }
}
impl Append for IrcLogWriter {
    fn append(&self, rec: &Record) -> Result<(), Box<::std::error::Error + Sync + Send>> {
        use log::Level::*;
        if rec.level() > self.level {
            return Ok(());
        }
        let colour = match rec.level() {
            Error => "04",
            Warn => "07",
            Info => "09",
            Debug => "10",
            Trace => "11"
        };
        self.sender.unbounded_send(
            ControlBotCommand::Log(format!("[\x0302{}\x0f] \x02\x03{}{}\x0f -- {}", rec.target(), colour, rec.level(), rec.args())))
            .unwrap();
        Ok(())
    }
    fn flush(&self) {
    }
}

fn main() -> Result<(), failure::Error> {
    eprintln!("[+] smsirc starting -- reading config file");
    let config_path = ::std::env::var("SMSIRC_CONFIG")
        .unwrap_or("config.toml".to_string());
    eprintln!("[+] config path: {} (set SMSIRC_CONFIG to change)", config_path);
    let config: Config = toml::from_str(&::std::fs::read_to_string(config_path)?)?;
    let stdout = ConsoleAppender::builder().build();
    let mut cm = ChannelMaker::new();
    eprintln!("[+] initialising better logging system");
    let cll = config.chan_loglevel.as_ref().map(|x| x as &str).unwrap_or("info").parse()?;
    let pll = config.stdout_loglevel.as_ref().map(|x| x as &str).unwrap_or("info").parse()?;
    let ilw = IrcLogWriter { sender: cm.cb_tx.clone(), level: cll };
    let log_config = LogConfig::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        .appender(Appender::builder().build("irc_chan", Box::new(ilw)))
        .logger(Logger::builder()
                .appender("irc_chan")
                .appender("stdout")
                .additive(false)
                .build("sms_irc", pll))
        .build(Root::builder().appender("stdout").build(pll))?;
    log4rs::init_config(log_config)?;
    if config.client.is_none() == config.insp_s2s.is_none() {
        error!("Config must contain either a [client] or an [insp_s2s] section (and not both)!");
        panic!("invalid configuration");
    }
    info!("Connecting to database");
    let store = Store::new(&config)?;
    info!("Initializing tokio");
    let mut core = Core::new()?;
    let hdl = core.handle();
    info!("Initializing modem");
    let mm = ModemManager::new(InitParameters {
        cfg: &config,
        cfg2: &(),
        store: store.clone(),
        cm: &mut cm,
        hdl: &hdl
    });
    hdl.spawn(mm.map_err(|e| {
        // FIXME: restartability

        error!("ModemManager failed: {}", e);
        panic!("modemmanager failed");
    }));
    let stream = Signal::new(SIGHUP).flatten_stream();
    hdl.spawn(stream.for_each(|i| {
        info!("Got signal {}", i);
        Ok(())
    }).map_err(|e| {
        error!("Signal handler failed: {}", e);
    }));
    info!("Initializing WhatsApp");
    let wa = WhatsappManager::new(InitParameters {
        cfg: &config,
        cfg2: &(),
        store: store.clone(),
        cm: &mut cm,
        hdl: &hdl
    });
    hdl.spawn(wa.map_err(|e| {
        // FIXME: restartability

        error!("WhatsappManager failed: {}", e);
        panic!("whatsapp failed");
    }));
    if config.client.is_some() {
        info!("Running in traditional IRC client mode");
        info!("Initializing control bot");
        let cb = core.run(ControlBot::new(InitParameters {
            cfg: &config,
            cfg2: config.client.as_ref().unwrap(),
            store: store.clone(),
            cm: &mut cm,
            hdl: &hdl
        }))?;
        hdl.spawn(cb.map_err(|e| {
            // FIXME: restartability

            error!("ControlBot failed: {}", e);
            panic!("controlbot failed");
        }));
        info!("Initializing contact factory");
        let cf = ContactFactory::new(config, store, cm, hdl);
        let _ = core.run(cf.map_err(|e| {
            error!("ContactFactory failed: {}", e);
            panic!("contactfactory failed");
        }));
    }
    else {
        info!("Running in InspIRCd s2s mode");
        let fut = core.run(InspLink::new(InitParameters {
            cfg: &config,
            cfg2: config.insp_s2s.as_ref().unwrap(),
            store: store.clone(),
            cm: &mut cm,
            hdl: &hdl
        }))?;
        let _ = core.run(fut.map_err(|e| {
            // FIXME: restartability

            error!("InspLink failed: {}", e);
            panic!("link failed");
        }));
    }
    Ok(())
}
