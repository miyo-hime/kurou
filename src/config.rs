use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TransportMode {
    Stdio,
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GatewayMode {
    Off,
    Presence,
    Mentions,
}

#[derive(Clone, Debug, Parser)]
#[command(author, version, about = "kurou - koma's read-window into discord")]
pub struct Config {
    #[arg(long, env = "TRANSPORT", default_value = "stdio")]
    pub transport: TransportMode,

    #[arg(long, env = "DISCORD_TOKEN")]
    pub discord_token: Option<String>,

    #[arg(long, env = "DISCORD_GUILD_ID")]
    pub discord_guild_id: Option<String>,

    // servers the crow may read but never speak in. when set, DISCORD_GUILD_ID is the
    // one place send_message is allowed to land.
    #[arg(long = "readonly-guild", env = "READONLY_GUILDS", value_delimiter = ',')]
    pub readonly_guilds: Vec<String>,

    // a separate observer bot for the read-only guilds. a different application than the
    // primary, so it can't post as koma there even if we fat-fingered it.
    #[arg(long = "readonly-token", env = "READONLY_DISCORD_TOKEN")]
    pub readonly_discord_token: Option<String>,

    #[arg(long, env = "PORT", default_value_t = 3000)]
    pub port: u16,

    #[arg(long, env = "HOST", default_value = "127.0.0.1")]
    pub host: IpAddr,

    #[arg(long = "allowed-host", env = "ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,

    #[arg(
        long = "allowed-origin",
        env = "ALLOWED_ORIGINS",
        value_delimiter = ','
    )]
    pub allowed_origins: Vec<String>,

    #[arg(long = "auth-token", env = "AUTH_TOKENS", value_delimiter = ',')]
    pub auth_tokens: Vec<String>,

    #[arg(long = "oauth-token-label", env = "OAUTH_TOKEN_LABEL")]
    pub oauth_token_label: Option<String>,

    #[arg(
        long = "public-base-url",
        env = "PUBLIC_BASE_URL",
        default_value = "http://127.0.0.1:3000"
    )]
    pub public_base_url: String,

    #[arg(long = "gateway-mode", env = "GATEWAY_MODE", default_value = "off")]
    pub gateway_mode: GatewayMode,

    // the watch-wall (nightwatch). opt-in: it forces the gateway on with message intents
    // and, with an observer bot, a second invisible gateway for the secondaries. http only.
    #[arg(long = "wall", env = "WALL", default_value_t = false, action = clap::ArgAction::Set)]
    pub wall: bool,

    // the crow's whole book lives in one sqlite file - mentions now, watch layout next,
    // phase-5 mod entries later. LEDGER_PATH is the name going forward; MENTION_DB_PATH
    // still works so a live unit doesn't orphan its db on upgrade. resolve via ledger_path().
    #[arg(long = "ledger-path", env = "LEDGER_PATH")]
    pub ledger_path: Option<PathBuf>,

    #[arg(
        long = "mention-keyword",
        env = "MENTION_KEYWORDS",
        value_delimiter = ',',
        default_value = "koma"
    )]
    pub mention_keywords: Vec<String>,
}

impl Config {
    pub fn ledger_path(&self) -> PathBuf {
        self.ledger_path
            .clone()
            .or_else(|| std::env::var_os("MENTION_DB_PATH").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("mentions.sqlite3"))
    }
}
