//! Simple logging system, to log to both the IRC channel and stdout.

use log::{Record, LevelFilter, Metadata};
use futures::sync::mpsc::UnboundedSender;
use crate::comm::ControlBotCommand;

pub struct Logger {
    chan_loglevel: LevelFilter,
    stdout_loglevel: LevelFilter,
    ignore_other_libraries: bool,
    sender: UnboundedSender<ControlBotCommand>,
}
impl Logger {
    pub fn new(cll: LevelFilter, sll: LevelFilter, ign: bool, sender: UnboundedSender<ControlBotCommand>) -> Self {
        Self {
            chan_loglevel: cll,
            stdout_loglevel: sll,
            ignore_other_libraries: ign,
            sender
        }
    }
    pub fn register(self) {
        let level = self.chan_loglevel.max(self.stdout_loglevel);
        let logger = Box::new(self);
        log::set_boxed_logger(logger)
            .unwrap();
        log::set_max_level(level);
    }
}
impl log::Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        if self.ignore_other_libraries && !metadata.target().starts_with("sms_irc") {
            return false;
        }
        let lvl = metadata.level();
        lvl <= self.chan_loglevel || lvl <= self.stdout_loglevel
    }
    fn log(&self, rec: &Record) {
        use log::Level::*;
        if rec.level() <= self.stdout_loglevel {
            println!("[{}] {} -- {}", rec.target(), rec.level(), rec.args());
        }
        if rec.level() <= self.chan_loglevel {
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
        }
    }
    fn flush(&self) {
    }
}
