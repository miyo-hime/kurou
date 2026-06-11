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
}
