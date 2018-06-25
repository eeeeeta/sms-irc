#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub irc_hostname: String,
    pub modem_path: String,
    pub admin_nick: String,
    pub irc_channel: String,
    #[serde(default)]
    pub control_bot_nick: Option<String>,
    #[serde(default)]
    pub cmgl_secs: Option<u32>,
    #[serde(default)]
    pub failure_interval: Option<u64>,
    #[serde(default)]
    pub chan_loglevel: Option<String>,
    #[serde(default)]
    pub stdout_loglevel: Option<String>,
    #[serde(default)]
    pub irc_port: Option<u16>,
    #[serde(default)]
    pub irc_password: Option<String>,
}

