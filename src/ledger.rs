use std::path::Path;

use anyhow::{Context, Result, bail};
use turso::{Builder, Database, Row, Value};

// the one place that knows the whole book. every tenant hands its schema over at open()
// and then draws a store off the shared database handle - one file, one engine, one owner.
#[derive(Clone)]
pub struct Ledger {
    db: Database,
}

impl Ledger {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let path_str = path.to_str().context("ledger path is not valid utf-8")?;

        // both experimental flags exist only to exorcise dbs built by 0.8.0-0.8.2: without
        // index_method a db that still carries msg_fts won't even open. drop with the migration.
        let db = Builder::new_local(path_str)
            .experimental_index_method(true)
            .experimental_vacuum(true)
            .build()
            .await
            .with_context(|| format!("failed to open ledger {path_str}"))?;

        let conn = connect(&db).context("failed to connect to ledger")?;
        conn.execute_batch(crate::mentions::SCHEMA).await.context("mentions schema")?;
        conn.execute_batch(crate::layout::SCHEMA).await.context("layout schema")?;
        conn.execute_batch(crate::archive::SCHEMA).await.context("archive schema")?;

        // the segment autopsy (2026-07-26): the fts index built one tantivy segment per
        // message forever, costing seconds-long write locks and hundreds of MB. gone.
        let haunted = {
            // the rows handle must die before vacuum - a live statement blocks it
            let mut fts = conn.query("select 1 from sqlite_master where name = 'msg_fts'", ()).await.context("check for fts index")?;
            fts.next().await.context("read fts check")?.is_some()
        };
        if haunted {
            tracing::info!("dropping legacy fts index msg_fts and reclaiming the segment graveyard");
            conn.execute("drop index msg_fts", ()).await.context("drop fts index")?;
            conn.execute("vacuum", ()).await.context("vacuum after fts drop")?;
        }

        Ok(Self { db })
    }

    pub fn mentions(&self) -> crate::mentions::MentionStore {
        crate::mentions::MentionStore::new(self.db.clone())
    }

    pub fn layout(&self) -> crate::layout::LayoutStore {
        crate::layout::LayoutStore::new(self.db.clone())
    }

    pub fn archive(&self) -> crate::archive::MessageStore {
        crate::archive::MessageStore::new(self.db.clone())
    }
}

// every tenant connects through here: turso's default busy handler fails instantly on
// lock contention, and that default once cost the archive ~150 messages a day.
pub(crate) fn connect(db: &Database) -> turso::Result<turso::Connection> {
    let conn = db.connect()?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

// turso hands back an owned Value per column; the tenants all speak text and int, so the
// unwrapping lives here once instead of five times.

pub(crate) fn null_text(value: Option<String>) -> Value {
    value.map(Value::Text).unwrap_or(Value::Null)
}

pub(crate) fn text(row: &Row, idx: usize) -> Result<String> {
    match row.get_value(idx)? {
        Value::Text(text) => Ok(text),
        Value::Null => Ok(String::new()),
        other => bail!("column {idx} expected text, got {other:?}"),
    }
}

pub(crate) fn opt_text(row: &Row, idx: usize) -> Result<Option<String>> {
    match row.get_value(idx)? {
        Value::Text(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        other => bail!("column {idx} expected text or null, got {other:?}"),
    }
}

pub(crate) fn int(row: &Row, idx: usize) -> Result<i64> {
    match row.get_value(idx)? {
        Value::Integer(value) => Ok(value),
        other => bail!("column {idx} expected integer, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // delete this test alongside the migration when the fts feature leaves Cargo.toml
    #[tokio::test]
    async fn open_exorcises_legacy_fts_index() {
        let path = std::env::temp_dir().join("kurou-fts-exorcism.db");
        let _ = std::fs::remove_file(&path);

        {
            let db = Builder::new_local(path.to_str().unwrap()).experimental_index_method(true).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute_batch(crate::archive::SCHEMA).await.unwrap();
            conn.execute("create index msg_fts on messages using fts (content)", ()).await.unwrap();
            conn.execute("insert into messages (message_id, channel_id, author_id, author_name, content, timestamp, payload) values (1, 'c', 'a', 'a', 'haunted content', 't', '{}')", ()).await.unwrap();
        }

        let ledger = Ledger::open(&path).await.unwrap();
        let conn = connect(&ledger.db).unwrap();
        let mut rows = conn.query("select 1 from sqlite_master where name = 'msg_fts'", ()).await.unwrap();
        assert!(rows.next().await.unwrap().is_none(), "fts index should be gone after open");

        let store = ledger.archive();
        let hits = store.search("haunted", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "pre-migration rows should survive the exorcism");

        let _ = std::fs::remove_file(&path);
    }
}
