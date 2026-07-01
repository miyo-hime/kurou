use anyhow::{Context, Result};
use serde::Serialize;
use turso::params::params_from_iter;
use turso::{Database, Value};

use crate::discord::types::RenderedMessage;
use crate::ledger::{int, opt_text, text};

// the crow's long memory. every message it hears lands here once, stored twice over: flat
// columns for filtering (author, content fts, mentions, snowflake range) and a json payload
// of the RenderedMessage so an archive-served scan renders byte-identical to a REST one.
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
    pub rendered: RenderedMessage,
    pub guild_id: Option<String>,
    pub channel_id: String,
    pub mention_ids: Vec<String>,
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

// a scan against the archive: same filters the REST sweep takes, resolved in one query.
#[derive(Clone, Debug, Default)]
pub struct ScanQuery {
    pub channel_id: String,
    pub author_ids: Vec<String>,
    pub mention_ids: Vec<String>,
    pub text: Option<String>,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub limit: u16,
}

pub struct ArchiveScan {
    pub matches: Vec<RenderedMessage>,
    // the oldest message id the archive holds for this channel - the coverage floor. below
    // it the archive is blind and only REST can see, so scan reports it so koma knows.
    pub floor: Option<i64>,
}

// fts index at init so every write going forward is indexed - beta turso's pluggable index
// captures inserts from the moment it exists, it does not backfill a table that predates it.
pub(crate) const SCHEMA: &str = r#"
    create table if not exists messages (
        message_id integer primary key,
        guild_id text,
        channel_id text not null,
        author_id text not null,
        author_name text not null,
        content text not null,
        mention_ids text not null default '',
        timestamp text not null,
        payload text not null,
        created_at text not null default current_timestamp
    );

    create index if not exists msg_fts on messages using fts (content);
    create index if not exists idx_messages_channel on messages(channel_id, message_id);
"#;

impl MessageStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn insert(&self, message: NewMessage) -> Result<bool> {
        let snowflake: i64 = message
            .rendered
            .id
            .parse()
            .with_context(|| format!("message id '{}' is not a snowflake", message.rendered.id))?;
        let payload = serde_json::to_string(&message.rendered).context("serialize payload")?;
        // space-bounded so a `like '% id %'` match can't collide 123 with 1234.
        let mention_ids = mention_haystack(&message.mention_ids);

        let conn = self.db.connect().context("archive connect")?;
        let changed = conn
            .execute(
                r#"
                insert or ignore into messages (
                    message_id, guild_id, channel_id, author_id, author_name,
                    content, mention_ids, timestamp, payload
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
                params_from_iter([
                    Value::Integer(snowflake),
                    crate::ledger::null_text(message.guild_id),
                    Value::Text(message.channel_id),
                    Value::Text(message.rendered.author_id.clone()),
                    Value::Text(message.rendered.author_name.clone()),
                    Value::Text(message.rendered.content.clone()),
                    Value::Text(mention_ids),
                    Value::Text(message.rendered.timestamp.clone()),
                    Value::Text(payload),
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
                params_from_iter([Value::Text(query.to_owned()), Value::Integer(i64::from(limit))]),
            )
            .await
            .context("search messages")?;

        let mut hits = Vec::new();
        while let Some(row) = rows.next().await.context("read message row")? {
            hits.push(MessageHit {
                message_id: int(&row, 0)?.to_string(),
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

    pub async fn scan(&self, query: ScanQuery) -> Result<ArchiveScan> {
        let conn = self.db.connect().context("archive connect")?;

        let mut sql = String::from("select payload from messages where channel_id = ?1");
        let mut params: Vec<Value> = vec![Value::Text(query.channel_id.clone())];

        if let Some(before) = query.before {
            params.push(Value::Integer(before));
            sql.push_str(&format!(" and message_id < ?{}", params.len()));
        }
        if let Some(after) = query.after {
            params.push(Value::Integer(after));
            sql.push_str(&format!(" and message_id > ?{}", params.len()));
        }
        if !query.author_ids.is_empty() {
            let start = params.len() + 1;
            for id in &query.author_ids {
                params.push(Value::Text(id.clone()));
            }
            let placeholders = (start..start + query.author_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" and author_id in ({placeholders})"));
        }
        if !query.mention_ids.is_empty() {
            // matches messages mentioning ANY of the wanted ids.
            let mut ors = Vec::new();
            for id in &query.mention_ids {
                params.push(Value::Text(format!(" {id} ")));
                ors.push(format!("mention_ids like '%' || ?{} || '%'", params.len()));
            }
            sql.push_str(&format!(" and ({})", ors.join(" or ")));
        }
        if let Some(text) = &query.text {
            params.push(Value::Text(format!("%{text}%")));
            sql.push_str(&format!(" and content like ?{}", params.len()));
        }

        params.push(Value::Integer(i64::from(query.limit)));
        sql.push_str(&format!(" order by message_id desc limit ?{}", params.len()));

        let mut rows = conn.query(sql, params_from_iter(params)).await.context("scan messages")?;
        let mut matches = Vec::new();
        while let Some(row) = rows.next().await.context("read scan row")? {
            let payload = text(&row, 0)?;
            let rendered: RenderedMessage =
                serde_json::from_str(&payload).context("deserialize payload")?;
            matches.push(rendered);
        }

        let mut floor_rows = conn
            .query(
                "select min(message_id) from messages where channel_id = ?1",
                params_from_iter([Value::Text(query.channel_id)]),
            )
            .await
            .context("scan floor")?;
        let floor = match floor_rows.next().await.context("read floor row")? {
            Some(row) => match row.get_value(0)? {
                Value::Integer(value) => Some(value),
                _ => None,
            },
            None => None,
        };

        Ok(ArchiveScan { matches, floor })
    }
}

fn mention_haystack(ids: &[String]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    format!(" {} ", ids.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn message(id: i64, author: &str, content: &str, mentions: &[&str]) -> NewMessage {
        NewMessage {
            rendered: RenderedMessage {
                id: id.to_string(),
                author_id: author.to_owned(),
                author_name: author.to_owned(),
                timestamp: "2026-07-01T00:00:00Z".to_owned(),
                edited_timestamp: None,
                reply: None,
                reactions: Vec::new(),
                attachments: Vec::new(),
                stickers: Vec::new(),
                embeds: Vec::new(),
                content: content.to_owned(),
            },
            guild_id: Some("guild".to_owned()),
            channel_id: "chan".to_owned(),
            mention_ids: mentions.iter().map(|id| id.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn archive_scan_filters_and_covers() {
        let path = std::env::temp_dir().join("kurou-archive-scan.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();

        assert!(store.insert(message(100, "koma", "the crow keeps a database", &["55"])).await.unwrap());
        assert!(store.insert(message(200, "kurone", "the cat sleeps", &["55", "66"])).await.unwrap());
        assert!(store.insert(message(300, "koma", "database again, newer", &[])).await.unwrap());
        assert!(!store.insert(message(100, "koma", "dupe", &[])).await.unwrap());

        // text filter, newest first
        let text = store
            .scan(ScanQuery { channel_id: "chan".into(), text: Some("database".into()), limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(text.matches.len(), 2);
        assert_eq!(text.matches[0].id, "300");
        assert_eq!(text.floor, Some(100));

        // author filter
        let by_author = store
            .scan(ScanQuery { channel_id: "chan".into(), author_ids: vec!["koma".into()], limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(by_author.matches.len(), 2);

        // mention filter - id 55, not the substring-colliding 5
        let by_mention = store
            .scan(ScanQuery { channel_id: "chan".into(), mention_ids: vec!["66".into()], limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(by_mention.matches.len(), 1);
        assert_eq!(by_mention.matches[0].id, "200");

        // before/after range (exclusive)
        let ranged = store
            .scan(ScanQuery { channel_id: "chan".into(), before: Some(300), after: Some(100), limit: 50, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(ranged.matches.len(), 1);
        assert_eq!(ranged.matches[0].id, "200");

        let _ = std::fs::remove_file(&path);
    }
}
