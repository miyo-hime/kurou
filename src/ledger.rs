use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn connect(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let connection = Connection::open(path)
        .with_context(|| format!("failed to open ledger {}", path.display()))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

pub async fn initialize(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || initialize_blocking(&path))
        .await
        .context("ledger init task panicked")?
}

// the one place that knows the whole book. every tenant hands over its create-table here.
fn initialize_blocking(path: &Path) -> Result<()> {
    let connection = connect(path)?;
    connection.execute_batch(crate::mentions::SCHEMA)?;
    Ok(())
}
