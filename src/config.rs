#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub database_url: String,
    #[serde(default)]
    pub client: Option<IrcClientConfig>,
    #[serde(default)]
    pub insp_s2s: Option<InspConfig>,
    #[serde(default)]
    pub irc_server: Option<IrcServerConfig>,
    #[serde(default)]
    pub modem: ModemConfig,
    #[serde(default)]
    pub whatsapp: WhatsappConfig,
    #[serde(default)]
    pub logging: LoggingConfig
}
#[derive(Deserialize, Debug, Clone, Default)]
pub struct LoggingConfig {
    #[serde(default)]
    pub chan_loglevel: Option<String>,
    #[serde(default)]
    pub stdout_loglevel: Option<String>,
    #[serde(default)]
    pub ignore_other_libraries: bool
}
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ModemConfig {
    pub modem_path: Option<String>,
    #[serde(default)]
    pub cmgl_secs: Option<u32>,
    #[serde(default)]
    pub restart_delay_ms: Option<u32>,
    #[serde(default)]
    pub restart_timeout_ms: Option<u32>,
    #[serde(default)]
    pub command_timeout_ms: Option<u32>,
}
#[derive(Deserialize, Debug, Clone, Default)]
pub struct WhatsappConfig {
    #[serde(default)]
    pub dl_path: Option<String>,
    #[serde(default)]
    pub media_path: Option<String>,
    #[serde(default)]
    pub autocreate_prefix: Option<String>,
    #[serde(default)]
    pub ack_check_interval: Option<u64>,
    #[serde(default)]
    pub ack_warn_ms: Option<u64>,
    #[serde(default)]
    pub ack_warn_pending_ms: Option<u64>,
    #[serde(default)]
    pub ack_expiry_ms: Option<u64>,
    #[serde(default)]
    pub ack_resend_ms: Option<u64>,
    #[serde(default)]
    pub backlog_start: Option<chrono::NaiveDateTime>
}
#[derive(Deserialize, Debug, Clone)]
pub struct IrcClientConfig {
    pub irc_hostname: String,
    pub admin_nick: String,
    pub irc_channel: String,
    #[serde(default)]
    pub control_bot_nick: Option<String>,
    #[serde(default)]
    pub failure_interval: Option<u64>,
    #[serde(default)]
    pub irc_port: Option<u16>,
    #[serde(default)]
    pub irc_password: Option<String>,
    #[serde(default)]
    pub webirc_password: Option<String>
}
#[derive(Deserialize, Debug, Clone)]
pub struct IrcServerConfig {
    pub listen: String
}
#[derive(Deserialize, Debug, Clone)]
pub struct InspConfig {
    pub sid: String,
    pub control_nick: String,
    pub admin_nick: String,
    pub sendpass: String,
    pub recvpass: String,
    pub server_desc: String,
    pub server_name: String,
    pub log_chan: String,
    pub hostname: String,
    pub port: u16,
    #[serde(default)]
    pub set_topics: bool,
    #[serde(default)]
    pub clobber_topics: bool,
    #[serde(default)]
    pub ensure_joined: bool
}
