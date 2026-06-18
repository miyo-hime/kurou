use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::params;

// the crow never reads the bento, it just keeps it. the browser owns the shape;
// here it's an opaque json blob in a single row that survives across her devices.
#[derive(Clone, Debug)]
pub struct LayoutStore {
    path: PathBuf,
}

pub(crate) const SCHEMA: &str = r#"
    create table if not exists watch_layout (
        id integer primary key check (id = 1),
        data text not null,
        updated_at text not null default current_timestamp
    );
"#;

impl LayoutStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn get(&self) -> Result<Option<String>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || get_blocking(&path))
            .await
            .context("layout get task panicked")?
    }

    pub async fn put(&self, data: String) -> Result<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || put_blocking(&path, data))
            .await
            .context("layout put task panicked")?
    }
}

fn get_blocking(path: &Path) -> Result<Option<String>> {
    let connection = crate::ledger::connect(path)?;
    let data = connection
        .query_row("select data from watch_layout where id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .ok();
    Ok(data)
}

fn put_blocking(path: &Path, data: String) -> Result<()> {
    let connection = crate::ledger::connect(path)?;
    connection.execute(
        r#"
        insert into watch_layout (id, data, updated_at)
        values (1, ?, current_timestamp)
        on conflict(id) do update set data = excluded.data, updated_at = current_timestamp
        "#,
        params![data],
    )?;
    Ok(())
}
