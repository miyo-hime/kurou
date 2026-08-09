use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use rusqlite::params;

// the crow never reads the bento, it just keeps it. the browser owns the shape;
// here it's an opaque json blob in a single row that survives across her devices.
#[derive(Clone, Debug)]
pub struct LayoutStore {
    path: Arc<PathBuf>,
}

pub(crate) const SCHEMA: &str = r#"
    create table if not exists watch_layout (
        id integer primary key check (id = 1),
        data text not null,
        updated_at text not null default current_timestamp
    );
"#;

impl LayoutStore {
    pub fn new(path: Arc<PathBuf>) -> Self {
        Self { path }
    }

    pub async fn get(&self) -> Result<Option<String>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("layout connect")?;
            conn.query_row("select data from watch_layout where id = 1", [], |row| row.get(0))
                .optional()
                .context("read layout")
        })
        .await
        .context("layout get task")?
    }

    pub async fn put(&self, data: String) -> Result<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("layout connect")?;
            conn.execute(
                r#"
                insert into watch_layout (id, data, updated_at)
                values (1, ?1, current_timestamp)
                on conflict(id) do update set data = excluded.data, updated_at = current_timestamp
                "#,
                params![data],
            )
            .context("write layout")?;
            Ok(())
        })
        .await
        .context("layout put task")?
    }
}
