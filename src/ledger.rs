use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::Connection;

// the one place that knows the whole book. every tenant hands its schema over at open()
// and then draws a store off the shared path - one file, one engine, connection per call.
#[derive(Clone, Debug)]
pub struct Ledger {
    path: Arc<PathBuf>,
}

impl Ledger {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let path = Arc::new(path.to_owned());
        let ledger = Self { path };

        let opening = ledger.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = connect(&opening.path).context("failed to open ledger")?;
            // wal is persistent - set once here, every later connection inherits it.
            conn.pragma_update(None, "journal_mode", "wal").context("set wal mode")?;
            conn.execute_batch(crate::mentions::SCHEMA).context("mentions schema")?;
            conn.execute_batch(crate::layout::SCHEMA).context("layout schema")?;

            // dbs from the turso era predate msg_fts, so a fresh index backfills from
            // the existing rows. rebuild on an empty table is free, so first boot is too.
            let fresh_fts = !table_exists(&conn, "msg_fts").context("check for fts table")?;
            conn.execute_batch(crate::archive::SCHEMA).context("archive schema")?;
            // pre-0.11 archives lack author_display; the duplicate-column error means it's already there
            if let Err(error) = conn.execute("alter table messages add column author_display text", [])
                && !error.to_string().contains("duplicate column") {
                return Err(error).context("add author_display column");
            }
            if let Err(error) = conn.execute("alter table messages add column deleted_at text", [])
                && !error.to_string().contains("duplicate column") {
                return Err(error).context("add deleted_at column");
            }
            if fresh_fts {
                tracing::info!("building the fts index over the archive");
                conn.execute("insert into msg_fts(msg_fts) values ('rebuild')", [])
                    .context("backfill fts index")?;
            }
            Ok(())
        })
        .await
        .context("ledger open task")??;

        Ok(ledger)
    }

    pub fn mentions(&self) -> crate::mentions::MentionStore {
        crate::mentions::MentionStore::new(self.path.clone())
    }

    pub fn layout(&self) -> crate::layout::LayoutStore {
        crate::layout::LayoutStore::new(self.path.clone())
    }

    pub fn archive(&self) -> crate::archive::MessageStore {
        crate::archive::MessageStore::new(self.path.clone())
    }
}

// every tenant connects through here: without a busy timeout, lock contention fails
// instantly, and that once cost the archive ~150 messages a day.
pub(crate) fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.prepare("select 1 from sqlite_master where name = ?1")?.exists([name])
}
