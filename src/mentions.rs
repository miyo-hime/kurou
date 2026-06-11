use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct MentionStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct NewMention {
    pub message_id: String,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub author_id: String,
    pub author_name: String,
    pub author_display_name: Option<String>,
    pub content: String,
    pub matched: String,
    pub timestamp: String,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MentionInfo {
    pub id: i64,
    pub message_id: String,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub author_id: String,
    pub author_name: String,
    pub author_display_name: Option<String>,
    pub content: String,
    pub matched: String,
    pub timestamp: String,
    pub link: String,
    pub seen: bool,
}

impl MentionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn initialize(&self) -> Result<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || initialize_blocking(&path))
            .await
            .context("mention store task panicked")?
    }

    pub async fn insert(&self, mention: NewMention) -> Result<bool> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || insert_blocking(&path, mention))
            .await
            .context("mention insert task panicked")?
    }

    pub async fn list(&self, include_seen: bool, limit: u8) -> Result<Vec<MentionInfo>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || list_blocking(&path, include_seen, limit))
            .await
            .context("mention list task panicked")?
    }

    pub async fn mark_seen(&self, ids: Option<Vec<i64>>) -> Result<usize> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || mark_seen_blocking(&path, ids))
            .await
            .context("mention mark-seen task panicked")?
    }
}

fn connect(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let connection = Connection::open(path)
        .with_context(|| format!("failed to open mention db {}", path.display()))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn initialize_blocking(path: &Path) -> Result<()> {
    let connection = connect(path)?;
    connection.execute_batch(
        r#"
        create table if not exists mentions (
            id integer primary key autoincrement,
            message_id text not null unique,
            guild_id text,
            channel_id text not null,
            author_id text not null,
            author_name text not null,
            author_display_name text,
            content text not null,
            matched text not null,
            timestamp text not null,
            link text not null,
            seen integer not null default 0,
            created_at text not null default current_timestamp
        );

        create index if not exists idx_mentions_seen_id on mentions(seen, id);
        "#,
    )?;
    Ok(())
}

fn insert_blocking(path: &Path, mention: NewMention) -> Result<bool> {
    let connection = connect(path)?;
    let changed = connection.execute(
        r#"
        insert or ignore into mentions (
            message_id,
            guild_id,
            channel_id,
            author_id,
            author_name,
            author_display_name,
            content,
            matched,
            timestamp,
            link
        ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        params![
            mention.message_id,
            mention.guild_id,
            mention.channel_id,
            mention.author_id,
            mention.author_name,
            mention.author_display_name,
            mention.content,
            mention.matched,
            mention.timestamp,
            mention.link,
        ],
    )?;
    Ok(changed > 0)
}

fn list_blocking(path: &Path, include_seen: bool, limit: u8) -> Result<Vec<MentionInfo>> {
    let connection = connect(path)?;
    let where_clause = if include_seen { "1 = 1" } else { "seen = 0" };
    let query = format!(
        r#"
        select
            id,
            message_id,
            guild_id,
            channel_id,
            author_id,
            author_name,
            author_display_name,
            content,
            matched,
            timestamp,
            link,
            seen
        from mentions
        where {where_clause}
        order by id desc
        limit ?
        "#
    );
    let mut statement = connection.prepare(&query)?;
    let rows = statement.query_map([i64::from(limit)], |row| {
        Ok(MentionInfo {
            id: row.get(0)?,
            message_id: row.get(1)?,
            guild_id: row.get(2)?,
            channel_id: row.get(3)?,
            author_id: row.get(4)?,
            author_name: row.get(5)?,
            author_display_name: row.get(6)?,
            content: row.get(7)?,
            matched: row.get(8)?,
            timestamp: row.get(9)?,
            link: row.get(10)?,
            seen: row.get::<_, i64>(11)? != 0,
        })
    })?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn mark_seen_blocking(path: &Path, ids: Option<Vec<i64>>) -> Result<usize> {
    let mut connection = connect(path)?;
    match ids {
        Some(ids) => {
            let transaction = connection.transaction()?;
            let mut changed = 0;
            for id in ids {
                changed += transaction.execute(
                    "update mentions set seen = 1 where id = ? and seen = 0",
                    [id],
                )?;
            }
            transaction.commit()?;
            Ok(changed)
        }
        None => Ok(connection.execute("update mentions set seen = 1 where seen = 0", [])?),
    }
}
