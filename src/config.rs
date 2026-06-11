use std::net::IpAddr;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TransportMode {
    Stdio,
    Http,
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
}
