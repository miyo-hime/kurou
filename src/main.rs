mod archive;
mod auth;
mod clock;
mod config;
#[cfg(test)]
mod contract_tests;
mod discord;
mod gateway;
mod layout;
mod ledger;
mod mentions;
mod modlog;
mod oauth;
mod scheduler;
mod server;
mod uploads;
mod wake;
mod wall;

use anyhow::Result;
use clap::Parser;
use config::{Config, TransportMode};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let config = Config::parse();

    match config.transport {
        TransportMode::Stdio => server::run_stdio(config).await,
        TransportMode::Http => server::run_http(config).await,
    }
}
