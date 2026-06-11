mod config;
mod discord;
mod server;

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
        TransportMode::Http => anyhow::bail!("http transport lands in phase 3, use stdio"),
    }
}
