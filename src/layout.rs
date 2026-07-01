use anyhow::{Context, Result};
use turso::params::params_from_iter;
use turso::{Database, Value};

use crate::ledger::text;

// the crow never reads the bento, it just keeps it. the browser owns the shape;
// here it's an opaque json blob in a single row that survives across her devices.
#[derive(Clone)]
pub struct LayoutStore {
    db: Database,
}

impl std::fmt::Debug for LayoutStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LayoutStore").finish_non_exhaustive()
    }
}

pub(crate) const SCHEMA: &str = r#"
    create table if not exists watch_layout (
        id integer primary key check (id = 1),
        data text not null,
        updated_at text not null default current_timestamp
    );
"#;

impl LayoutStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn get(&self) -> Result<Option<String>> {
        let conn = self.db.connect().context("layout connect")?;
        let mut rows = conn
            .query("select data from watch_layout where id = 1", ())
            .await
            .context("read layout")?;
        match rows.next().await.context("read layout row")? {
            Some(row) => Ok(Some(text(&row, 0)?)),
            None => Ok(None),
        }
    }

    pub async fn put(&self, data: String) -> Result<()> {
        let conn = self.db.connect().context("layout connect")?;
        conn.execute(
            r#"
            insert into watch_layout (id, data, updated_at)
            values (1, ?1, current_timestamp)
            on conflict(id) do update set data = excluded.data, updated_at = current_timestamp
            "#,
            params_from_iter([Value::Text(data)]),
        )
        .await
        .context("write layout")?;
        Ok(())
    }
}
