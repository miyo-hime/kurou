use anyhow::{Context, Result};
use serde::Serialize;
use turso::params::params_from_iter;
use turso::{Database, Value};

use crate::ledger::{int, null_text, opt_text, text};

#[derive(Clone)]
pub struct MentionStore {
    db: Database,
}

impl std::fmt::Debug for MentionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("MentionStore").finish_non_exhaustive()
    }
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
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn insert(&self, mention: NewMention) -> Result<bool> {
        let conn = self.db.connect().context("mention connect")?;
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
                params_from_iter([
                    Value::Text(mention.message_id),
                    null_text(mention.guild_id),
                    Value::Text(mention.channel_id),
                    Value::Text(mention.author_id),
                    Value::Text(mention.author_name),
                    null_text(mention.author_display_name),
                    Value::Text(mention.content),
                    Value::Text(mention.matched),
                    Value::Text(mention.timestamp),
                    Value::Text(mention.link),
                ]),
            )
            .await
            .context("insert mention")?;
        Ok(changed > 0)
    }

    pub async fn list(&self, include_seen: bool, limit: u8) -> Result<Vec<MentionInfo>> {
        let conn = self.db.connect().context("mention connect")?;
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

        let mut rows = conn
            .query(query, params_from_iter([Value::Integer(i64::from(limit))]))
            .await
            .context("list mentions")?;

        let mut mentions = Vec::new();
        while let Some(row) = rows.next().await.context("read mention row")? {
            mentions.push(MentionInfo {
                id: int(&row, 0)?,
                message_id: text(&row, 1)?,
                guild_id: opt_text(&row, 2)?,
                channel_id: text(&row, 3)?,
                author_id: text(&row, 4)?,
                author_name: text(&row, 5)?,
                author_display_name: opt_text(&row, 6)?,
                content: text(&row, 7)?,
                matched: text(&row, 8)?,
                timestamp: text(&row, 9)?,
                link: text(&row, 10)?,
                seen: int(&row, 11)? != 0,
            });
        }
        Ok(mentions)
    }

    pub async fn mark_seen(&self, ids: Option<Vec<i64>>) -> Result<usize> {
        let conn = self.db.connect().context("mention connect")?;
        let changed = match ids {
            Some(ids) => {
                let mut changed = 0;
                for id in ids {
                    changed += conn
                        .execute(
                            "update mentions set seen = 1 where id = ?1 and seen = 0",
                            params_from_iter([Value::Integer(id)]),
                        )
                        .await
                        .context("mark mention seen")?;
                }
                changed
            }
            None => conn
                .execute("update mentions set seen = 1 where seen = 0", ())
                .await
                .context("mark all mentions seen")?,
        };
        Ok(changed as usize)
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
