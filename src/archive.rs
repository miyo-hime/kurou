use anyhow::{Context, Result};
use serde::Serialize;
use turso::params::params_from_iter;
use turso::{Database, Value};

use crate::ledger::{null_text, opt_text, text};

// the crow's long memory. every message it hears on the wire lands here once, and the
// fts index over content is what turns "did anyone ever say X" into a local lookup
// instead of a paged REST crawl back through discord.
#[derive(Clone)]
pub struct MessageStore {
    db: Database,
}

impl std::fmt::Debug for MessageStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("MessageStore").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct NewMessage {
    pub message_id: String,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub author_id: String,
    pub author_name: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MessageHit {
    pub message_id: String,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub author_id: String,
    pub author_name: String,
    pub content: String,
    pub timestamp: String,
}

// fts index at init so every write going forward is indexed - beta turso's pluggable index
// captures inserts from the moment it exists, it does not backfill a table that predates it.
pub(crate) const SCHEMA: &str = r#"
    create table if not exists messages (
        id integer primary key autoincrement,
        message_id text not null unique,
        guild_id text,
        channel_id text not null,
        author_id text not null,
        author_name text not null,
        content text not null,
        timestamp text not null,
        created_at text not null default current_timestamp
    );

    create index if not exists msg_fts on messages using fts (content);
"#;

impl MessageStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn insert(&self, message: NewMessage) -> Result<bool> {
        let conn = self.db.connect().context("archive connect")?;
        let changed = conn
            .execute(
                r#"
                insert or ignore into messages (
                    message_id, guild_id, channel_id, author_id, author_name, content, timestamp
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
                params_from_iter([
                    Value::Text(message.message_id),
                    null_text(message.guild_id),
                    Value::Text(message.channel_id),
                    Value::Text(message.author_id),
                    Value::Text(message.author_name),
                    Value::Text(message.content),
                    Value::Text(message.timestamp),
                ]),
            )
            .await
            .context("insert message")?;
        Ok(changed > 0)
    }

    pub async fn search(&self, query: &str, limit: u8) -> Result<Vec<MessageHit>> {
        let conn = self.db.connect().context("archive connect")?;
        let mut rows = conn
            .query(
                r#"
                select message_id, guild_id, channel_id, author_id, author_name, content, timestamp
                from messages
                where fts_match(content, ?1)
                order by fts_score(content, ?1) desc
                limit ?2
                "#,
                params_from_iter([
                    Value::Text(query.to_owned()),
                    Value::Integer(i64::from(limit)),
                ]),
            )
            .await
            .context("search messages")?;

        let mut hits = Vec::new();
        while let Some(row) = rows.next().await.context("read message row")? {
            hits.push(MessageHit {
                message_id: text(&row, 0)?,
                guild_id: opt_text(&row, 1)?,
                channel_id: text(&row, 2)?,
                author_id: text(&row, 3)?,
                author_name: text(&row, 4)?,
                content: text(&row, 5)?,
                timestamp: text(&row, 6)?,
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn sample(id: &str, author: &str, content: &str) -> NewMessage {
        NewMessage {
            message_id: id.to_owned(),
            guild_id: Some("guild".to_owned()),
            channel_id: "channel".to_owned(),
            author_id: author.to_owned(),
            author_name: author.to_owned(),
            content: content.to_owned(),
            timestamp: "2026-07-01T00:00:00Z".to_owned(),
        }
    }

    // the load-bearing assumption: every store op opens its own connection off the shared
    // Database, so a write on one connection must be visible to fts on another.
    #[tokio::test]
    async fn archive_search_across_connections() {
        let path = std::env::temp_dir().join("kurou-archive-across-conn.db");
        let _ = std::fs::remove_file(&path);
        let ledger = Ledger::open(&path).await.unwrap();
        let store = ledger.archive();

        assert!(store.insert(sample("1", "koma", "crow archives every message into a database")).await.unwrap());
        assert!(store.insert(sample("2", "kurone", "the cat is asleep on the rack")).await.unwrap());
        // unique message_id: the second sight of a message is a no-op, not a dupe row.
        assert!(!store.insert(sample("1", "koma", "crow archives every message into a database")).await.unwrap());

        let hits = store.search("database", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "fts should match exactly the one message that says database");
        assert_eq!(hits[0].message_id, "1");

        let _ = std::fs::remove_file(&path);
    }
}
