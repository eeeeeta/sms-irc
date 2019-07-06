#![allow(proc_macro_derive_resolution_fallback)]

#[macro_use] extern crate diesel;
#[macro_use] extern crate serde_derive;
#[macro_use] extern crate failure;
#[macro_use] extern crate log;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate diesel_migrations;
extern crate whatsappweb_eta as whatsappweb;

mod config;
mod logging;
mod store;
mod modem;
mod comm;
#[macro_use]
mod util;
mod schema;
mod models;
mod contact;
mod contact_factory;
mod contact_common;
mod admin;
mod control;
mod control_common;
mod sender_common;
mod whatsapp;
mod whatsapp_media;
mod insp_s2s;
mod insp_user;
mod irc_s2c;
mod irc_s2c_registration;
mod irc_s2c_v3;

use crate::config::Config;
use crate::store::Store;
use crate::modem::ModemManager;
use crate::control::ControlBot;
use crate::comm::{ChannelMaker, InitParameters};
use futures::{Future, Stream};
use crate::contact_factory::ContactFactory;
use tokio_core::reactor::Core;
use crate::insp_s2s::InspLink;
use crate::irc_s2c::IrcServer;
use crate::whatsapp::WhatsappManager;
use tokio_signal::unix::{Signal, SIGHUP};
use std::path::Path;

fn main() -> Result<(), failure::Error> {
    eprintln!("[*] sms-irc version {}", env!("CARGO_PKG_VERSION"));
    eprintln!("[*] an eta project <https://theta.eu.org>\n");

    let config_path = ::std::env::var("SMSIRC_CONFIG");
    let is_def = config_path.is_ok();
    let config_path = match config_path {
        Ok(p) => p,
        _ => {
            if Path::new("config.toml").exists() {
                "config.toml".to_string()
            }
            else if Path::new("/etc/sms-irc.conf").exists() {
                "/etc/sms-irc.conf".to_string()
            }
            else {
                eprintln!("[!] *** I can't find a configuration file! ***");
                eprintln!("[!] Please place one either at ./config.toml or /etc/sms-irc.conf...");
                eprintln!("[!] ...or set the SMSIRC_CONFIG environment variable to point to one!");
                panic!("Cannot find a configuration file!");
            }
        }
    };
    eprintln!("[+] Reading configuration file from file '{}'...", config_path);
    if !is_def {
        eprintln!("[*] (Set the SMSIRC_CONFIG environment variable to change this.)");
    }
    let config: Config = toml::from_str(&::std::fs::read_to_string(config_path)?)?;

    let mut cm = ChannelMaker::new();

    eprintln!("[+] Initializing better logging system...");
    let cll = config.logging.chan_loglevel
        .as_ref().map(|x| x as &str).unwrap_or("info").parse()?;
    let sll = config.logging.stdout_loglevel
        .as_ref().map(|x| x as &str).unwrap_or("info").parse()?;
    let ign = config.logging.ignore_other_libraries;
    eprintln!("[*] IRC loglevel: {} | stdout loglevel: {}", cll, sll);
    let logger = logging::Logger::new(cll, sll, ign, cm.cb_tx.clone());
    logger.register();

    info!("sms-irc version {}", env!("CARGO_PKG_VERSION"));
    let configs = (config.client.is_some() as u32)
        + (config.insp_s2s.is_some() as u32)
        + (config.irc_server.is_some() as u32);

    if configs != 1 {
        error!("Config must have EXACTLY ONE [client], [insp_s2s] or [irc_server] section!");
        panic!("invalid configuration");
    }

    info!("Connecting to PostgreSQL");
    let store = Store::new(&config)?;
    debug!("Initializing tokio");
    let mut core = Core::new()?;
    let hdl = core.handle();
    debug!("Initializing modem");
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
    debug!("Initializing WhatsApp");
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
        debug!("Initializing control bot");
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
        debug!("Initializing contact factory");
        let cf = ContactFactory::new(config, store, cm, hdl);
        let _ = core.run(cf.map_err(|e| {
            error!("ContactFactory failed: {}", e);
            panic!("contactfactory failed");
        }));
    }
    else if config.insp_s2s.is_some() {
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
    else if config.irc_server.is_some() {
        info!("Running in IRC server mode");
        let fut = IrcServer::new(InitParameters {
            cfg: &config,
            cfg2: config.irc_server.as_ref().unwrap(),
            store: store.clone(),
            cm: &mut cm,
            hdl: &hdl
        })?;
        let _ = core.run(fut.map_err(|e| {
            // FIXME: restartability

            error!("IrcServer failed: {}", e);
            panic!("server failed");
        }));
    }
    Ok(())
}
