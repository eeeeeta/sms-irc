#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub modem_path: Option<String>,
    #[serde(default)]
    pub cmgl_secs: Option<u32>,
    #[serde(default)]
    pub modem_restart_delay_ms: Option<u32>,
    #[serde(default)]
    pub modem_restart_timeout_ms: Option<u32>,
    #[serde(default)]
    pub chan_loglevel: Option<String>,
    #[serde(default)]
    pub stdout_loglevel: Option<String>,
    #[serde(default)]
    pub qr_path: Option<String>,
    #[serde(default)]
    pub dl_path: Option<String>,
    #[serde(default)]
    pub media_path: Option<String>,
    #[serde(default)]
    pub client: Option<IrcClientConfig>,
    #[serde(default)]
    pub insp_s2s: Option<InspConfig>,
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
    pub clobber_topics: bool
}
