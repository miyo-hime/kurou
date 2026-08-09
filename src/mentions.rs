use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::params;
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct MentionStore {
    path: Arc<PathBuf>,
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
    pub fn new(path: Arc<PathBuf>) -> Self {
        Self { path }
    }

    pub async fn insert(&self, mention: NewMention) -> Result<bool> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("mention connect")?;
            let changed = conn
                .execute(
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
                    ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
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
                )
                .context("insert mention")?;
            Ok(changed > 0)
        })
        .await
        .context("mention insert task")?
    }

    pub async fn list(&self, include_seen: bool, limit: u8) -> Result<Vec<MentionInfo>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("mention connect")?;
            let where_clause = if include_seen { "1 = 1" } else { "seen = 0" };
            let query = format!(
                r#"
                select
                    id, message_id, guild_id, channel_id, author_id, author_name,
                    author_display_name, content, matched, timestamp, link, seen
                from mentions
                where {where_clause}
                order by id desc
                limit ?1
                "#
            );

            let mut stmt = conn.prepare(&query).context("prepare list")?;
            let mentions = stmt
                .query_map(params![i64::from(limit)], |row| {
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
                })
                .context("list mentions")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read mention rows")?;
            Ok(mentions)
        })
        .await
        .context("mention list task")?
    }

    pub async fn mark_seen(&self, ids: Option<Vec<i64>>) -> Result<usize> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("mention connect")?;
            let changed = match ids {
                Some(ids) => {
                    let mut changed = 0;
                    for id in ids {
                        changed += conn
                            .execute("update mentions set seen = 1 where id = ?1 and seen = 0", params![id])
                            .context("mark mention seen")?;
                    }
                    changed
                }
                None => conn
                    .execute("update mentions set seen = 1 where seen = 0", [])
                    .context("mark all mentions seen")?,
            };
            Ok(changed)
        })
        .await
        .context("mention mark task")?
    }
}

pub(crate) const SCHEMA: &str = r#"
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
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn sample(id: &str) -> NewMention {
        NewMention {
            message_id: id.to_owned(),
            guild_id: Some("guild".to_owned()),
            channel_id: "channel".to_owned(),
            author_id: "author".to_owned(),
            author_name: "kurone".to_owned(),
            author_display_name: None,
            content: "koma look at this".to_owned(),
            matched: "koma".to_owned(),
            timestamp: "2026-07-01T00:00:00Z".to_owned(),
            link: "https://discord.com/x".to_owned(),
        }
    }

    #[tokio::test]
    async fn mention_inbox_roundtrip() {
        let path = std::env::temp_dir().join("kurou-mention-roundtrip.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().mentions();

        assert!(store.insert(sample("1")).await.unwrap());
        assert!(store.insert(sample("2")).await.unwrap());
        assert!(!store.insert(sample("1")).await.unwrap(), "unique message_id dedups");

        let unseen = store.list(false, 20).await.unwrap();
        assert_eq!(unseen.len(), 2);
        assert_eq!(unseen[0].message_id, "2", "newest first");
        assert!(unseen.iter().all(|mention| !mention.seen));

        assert_eq!(store.mark_seen(Some(vec![unseen[0].id])).await.unwrap(), 1);
        assert_eq!(store.list(false, 20).await.unwrap().len(), 1);
        assert_eq!(store.list(true, 20).await.unwrap().len(), 2);

        let _ = std::fs::remove_file(&path);
    }
}
