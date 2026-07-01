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

        let db = Builder::new_local(path_str)
            .experimental_index_method(true) // fts rides on the pluggable index method
            .build()
            .await
            .with_context(|| format!("failed to open ledger {path_str}"))?;

        let conn = db.connect().context("failed to connect to ledger")?;
        conn.execute_batch(crate::mentions::SCHEMA).await.context("mentions schema")?;
        conn.execute_batch(crate::layout::SCHEMA).await.context("layout schema")?;
        conn.execute_batch(crate::archive::SCHEMA).await.context("archive schema")?;

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
