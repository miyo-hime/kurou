use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use rusqlite::types::Value;
use serde::Serialize;

use crate::discord::types::RenderedMessage;

// the crow's long memory. every message it hears lands here once, stored twice over: flat
// columns for filtering (author, content, mentions, snowflake range) and a json payload
// of the RenderedMessage so an archive-served scan renders byte-identical to a REST one.
#[derive(Clone, Debug)]
pub struct MessageStore {
    path: Arc<PathBuf>,
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
    pub author_display: Option<String>,
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

pub(crate) const SCHEMA: &str = r#"
    create table if not exists messages (
        message_id integer primary key,
        guild_id text,
        channel_id text not null,
        author_id text not null,
        author_name text not null,
        author_display text,
        content text not null,
        mention_ids text not null default '',
        timestamp text not null,
        payload text not null,
        created_at text not null default current_timestamp,
        deleted_at text
    );

    create table if not exists message_edits (
        message_id integer not null,
        content text not null,
        superseded_at text not null default current_timestamp
    );

    create index if not exists idx_messages_channel on messages(channel_id, message_id);

    create virtual table if not exists msg_fts using fts5(content, content='messages', content_rowid='message_id');

    create trigger if not exists msg_fts_ai after insert on messages begin
        insert into msg_fts(rowid, content) values (new.message_id, new.content);
    end;
    create trigger if not exists msg_fts_ad after delete on messages begin
        insert into msg_fts(msg_fts, rowid, content) values ('delete', old.message_id, old.content);
    end;
    create trigger if not exists msg_fts_au after update on messages begin
        insert into msg_fts(msg_fts, rowid, content) values ('delete', old.message_id, old.content);
        insert into msg_fts(rowid, content) values (new.message_id, new.content);
    end;
"#;

impl MessageStore {
    pub fn new(path: Arc<PathBuf>) -> Self {
        Self { path }
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

        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("archive connect")?;
            let changed = conn
                .execute(
                    r#"
                    insert or ignore into messages (
                        message_id, guild_id, channel_id, author_id, author_name, author_display,
                        content, mention_ids, timestamp, payload
                    ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                    "#,
                    params![
                        snowflake,
                        message.guild_id,
                        message.channel_id,
                        message.rendered.author_id,
                        message.rendered.author_name,
                        message.rendered.author_display,
                        message.rendered.content,
                        mention_ids,
                        message.rendered.timestamp,
                        payload,
                    ],
                )
                .context("insert message")?;
            Ok(changed > 0)
        })
        .await
        .context("archive insert task")?
    }

    pub async fn delete(&self, message_id: u64) -> Result<bool> {
        Ok(self.delete_bulk(vec![message_id]).await? > 0)
    }

    pub async fn delete_bulk(&self, message_ids: Vec<u64>) -> Result<usize> {
        let snowflakes = message_ids
            .into_iter()
            .map(|id| i64::try_from(id).with_context(|| format!("message id '{id}' does not fit sqlite integer")))
            .collect::<Result<Vec<_>>>()?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = crate::ledger::connect(&path).context("archive connect")?;
            let tx = conn.transaction().context("begin message delete")?;
            let mut changed = 0;
            for snowflake in snowflakes {
                changed += tx
                    .execute(
                        "update messages set deleted_at = current_timestamp where message_id = ?1 and deleted_at is null",
                        [snowflake],
                    )
                    .context("mark message deleted")?;
            }
            tx.commit().context("commit message delete")?;
            Ok(changed)
        })
        .await
        .context("archive delete task")?
    }

    pub async fn edit(&self, message_id: u64, content: Option<String>, edited_timestamp: Option<String>) -> Result<bool> {
        let Some(content) = content else {
            return Ok(false);
        };
        let snowflake = i64::try_from(message_id)
            .with_context(|| format!("message id '{message_id}' does not fit sqlite integer"))?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = crate::ledger::connect(&path).context("archive connect")?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).context("begin message edit")?;
            let previous = tx
                .query_row(
                    "select content, payload from messages where message_id = ?1",
                    [snowflake],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .context("read message before edit")?;
            let Some((previous_content, payload)) = previous else {
                return Ok(false);
            };
            if previous_content == content {
                return Ok(false);
            }

            let mut rendered: RenderedMessage = serde_json::from_str(&payload).context("deserialize payload before edit")?;
            rendered.content = content.clone();
            rendered.edited_timestamp = edited_timestamp.clone();
            let payload = serde_json::to_string(&rendered).context("serialize edited payload")?;

            tx.execute(
                "insert into message_edits (message_id, content, superseded_at) values (?1, ?2, coalesce(?3, current_timestamp))",
                params![snowflake, previous_content, edited_timestamp],
            )
            .context("save previous message draft")?;
            tx.execute(
                "update messages set content = ?2, payload = ?3 where message_id = ?1",
                params![snowflake, content, payload],
            )
            .context("update edited message")?;
            tx.commit().context("commit message edit")?;
            Ok(true)
        })
        .await
        .context("archive edit task")?
    }

    pub async fn search(&self, query: &str, limit: u8) -> Result<Vec<MessageHit>> {
        let match_query = fts_query(query)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("archive connect")?;
            let mut stmt = conn
                .prepare(
                    r#"
                    select m.message_id, m.guild_id, m.channel_id, m.author_id, m.author_name, m.author_display, m.content, m.timestamp, m.deleted_at is not null
                    from msg_fts join messages m on m.message_id = msg_fts.rowid
                    where msg_fts match ?1
                    order by m.message_id desc
                    limit ?2
                    "#,
                )
                .context("prepare search")?;
            let hits = stmt
                .query_map(params![match_query, i64::from(limit)], |row| {
                    Ok(MessageHit {
                        message_id: row.get::<_, i64>(0)?.to_string(),
                        guild_id: row.get(1)?,
                        channel_id: row.get(2)?,
                        author_id: row.get(3)?,
                        author_name: row.get(4)?,
                        author_display: row.get(5)?,
                        content: deleted_content(row.get(6)?, row.get(8)?),
                        timestamp: row.get(7)?,
                    })
                })
                .context("search messages")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read message rows")?;
            Ok(hits)
        })
        .await
        .context("archive search task")?
    }

    pub async fn scan(&self, query: ScanQuery) -> Result<ArchiveScan> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("archive connect")?;

            let mut sql = String::from("select payload, deleted_at is not null from messages where channel_id = ?1");
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
                params.push(Value::Text(like_pattern(text)));
                sql.push_str(&format!(" and content like ?{} escape '\\'", params.len()));
            }

            params.push(Value::Integer(i64::from(query.limit)));
            sql.push_str(&format!(" order by message_id desc limit ?{}", params.len()));

            let mut stmt = conn.prepare(&sql).context("prepare scan")?;
            let payloads = stmt
                .query_map(rusqlite::params_from_iter(params), |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))
                .context("scan messages")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read scan rows")?;
            let matches = payloads
                .iter()
                .map(|(payload, deleted)| {
                    let mut rendered: RenderedMessage = serde_json::from_str(payload).context("deserialize payload")?;
                    if *deleted {
                        rendered.content = deleted_content(rendered.content, true);
                    }
                    Ok(rendered)
                })
                .collect::<Result<Vec<_>>>()?;

            let floor = conn
                .query_row(
                    "select min(message_id) from messages where channel_id = ?1",
                    params![query.channel_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .context("scan floor")?;

            Ok(ArchiveScan { matches, floor })
        })
        .await
        .context("archive scan task")?
    }
}

// fts5 match syntax has operators and bare tokens error on punctuation, so every word
// becomes a quoted prefix token: `koma database` -> `"koma"* "database"*` (AND semantics).
fn fts_query(query: &str) -> Result<String> {
    let tokens: Vec<String> =
        query.split_whitespace().map(|token| format!("\"{}\"*", token.replace('"', "\"\""))).collect();
    if tokens.is_empty() {
        bail!("search query is empty");
    }
    Ok(tokens.join(" "))
}

fn like_pattern(text: &str) -> String {
    format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn deleted_content(content: String, deleted: bool) -> String {
    match (deleted, content.is_empty()) {
        (true, true) => "[deleted]".to_string(),
        (true, false) => format!("[deleted] {content}"),
        (false, _) => content,
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
                author_display: None,
                timestamp: "2026-07-01T00:00:00Z".to_owned(),
                edited_timestamp: None,
                kind: None,
                reply: None,
                forwarded: None,
                poll: None,
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
    async fn delete_marks_and_renders_the_archived_message() {
        let path = std::env::temp_dir().join("kurou-archive-delete.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();
        store.insert(message(100, "koma", "a doomed draft", &[])).await.unwrap();

        assert!(store.delete(100).await.unwrap());
        assert!(!store.delete(100).await.unwrap());

        let conn = crate::ledger::connect(&path).unwrap();
        let deleted_at: Option<String> = conn
            .query_row("select deleted_at from messages where message_id = 100", [], |row| row.get(0))
            .unwrap();
        assert!(deleted_at.is_some());
        drop(conn);

        let hits = store.search("doomed", 10).await.unwrap();
        assert_eq!(hits[0].content, "[deleted] a doomed draft");
        let scan = store.scan(ScanQuery { channel_id: "chan".into(), limit: 10, ..Default::default() }).await.unwrap();
        assert!(crate::discord::types::render_messages(&scan.matches).contains("[deleted] a doomed draft"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn bulk_delete_marks_every_known_row() {
        let path = std::env::temp_dir().join("kurou-archive-bulk-delete.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();
        store.insert(message(100, "koma", "first", &[])).await.unwrap();
        store.insert(message(200, "koma", "second", &[])).await.unwrap();
        store.insert(message(300, "koma", "third", &[])).await.unwrap();

        assert_eq!(store.delete_bulk(vec![100, 300, 999]).await.unwrap(), 2);
        let scan = store.scan(ScanQuery { channel_id: "chan".into(), limit: 10, ..Default::default() }).await.unwrap();
        assert_eq!(scan.matches[0].content, "[deleted] third");
        assert_eq!(scan.matches[1].content, "second");
        assert_eq!(scan.matches[2].content, "[deleted] first");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn edit_replaces_content_and_keeps_the_previous_draft() {
        let path = std::env::temp_dir().join("kurou-archive-edit.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();
        store.insert(message(100, "koma", "first draft", &[])).await.unwrap();
        let edited_at = "2026-09-06T07:00:00Z";

        assert!(store.edit(100, Some("final draft".into()), Some(edited_at.into())).await.unwrap());
        assert!(!store.edit(100, Some("final draft".into()), Some(edited_at.into())).await.unwrap());
        assert!(store.search("first", 10).await.unwrap().is_empty());
        assert_eq!(store.search("final", 10).await.unwrap()[0].content, "final draft");

        let conn = crate::ledger::connect(&path).unwrap();
        let (content, payload): (String, String) = conn
            .query_row("select content, payload from messages where message_id = 100", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(content, "final draft");
        let rendered: RenderedMessage = serde_json::from_str(&payload).unwrap();
        assert_eq!(rendered.content, "final draft");
        assert_eq!(rendered.edited_timestamp.as_deref(), Some(edited_at));
        let drafts: Vec<(String, String)> = conn
            .prepare("select content, superseded_at from message_edits where message_id = 100")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(drafts, vec![("first draft".into(), edited_at.into())]);
        drop(conn);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn contentless_update_changes_nothing() {
        let path = std::env::temp_dir().join("kurou-archive-contentless-update.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();
        store.insert(message(100, "koma", "link with a resolving embed", &[])).await.unwrap();

        assert!(!store.edit(100, None, Some("2026-09-06T07:00:00Z".into())).await.unwrap());
        let conn = crate::ledger::connect(&path).unwrap();
        let content: String = conn.query_row("select content from messages where message_id = 100", [], |row| row.get(0)).unwrap();
        let edits: i64 = conn.query_row("select count(*) from message_edits", [], |row| row.get(0)).unwrap();
        assert_eq!(content, "link with a resolving embed");
        assert_eq!(edits, 0);
        drop(conn);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn old_archive_schema_migrates_edits_and_deletions() {
        let path = std::env::temp_dir().join("kurou-archive-edit-delete-migration.db");
        let _ = std::fs::remove_file(&path);
        let archived = message(100, "koma", "old schema", &[]).rendered;
        let payload = serde_json::to_string(&archived).unwrap();
        let conn = crate::ledger::connect(&path).unwrap();
        conn.execute_batch(
            r#"
            create table messages (
                message_id integer primary key,
                guild_id text,
                channel_id text not null,
                author_id text not null,
                author_name text not null,
                author_display text,
                content text not null,
                mention_ids text not null default '',
                timestamp text not null,
                payload text not null,
                created_at text not null default current_timestamp
            );
            "#,
        )
        .unwrap();
        conn.execute(
            "insert into messages (message_id, guild_id, channel_id, author_id, author_name, content, timestamp, payload) values (100, 'guild', 'chan', 'koma', 'koma', 'old schema', '2026-07-01T00:00:00Z', ?1)",
            [payload],
        )
        .unwrap();
        drop(conn);

        let store = Ledger::open(&path).await.unwrap().archive();
        assert!(store.edit(100, Some("new schema".into()), Some("2026-09-06T07:00:00Z".into())).await.unwrap());
        assert!(store.delete(100).await.unwrap());
        let conn = crate::ledger::connect(&path).unwrap();
        let deleted_at: Option<String> = conn.query_row("select deleted_at from messages where message_id = 100", [], |row| row.get(0)).unwrap();
        let edits: i64 = conn.query_row("select count(*) from message_edits where message_id = 100", [], |row| row.get(0)).unwrap();
        assert!(deleted_at.is_some());
        assert_eq!(edits, 1);
        drop(conn);

        let _ = std::fs::remove_file(&path);
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

    #[tokio::test]
    async fn archive_insert_survives_reopen() {
        let path = std::env::temp_dir().join("kurou-reopen-repro.db");
        let _ = std::fs::remove_file(&path);

        {
            let store = Ledger::open(&path).await.unwrap().archive();
            store.insert(message(100, "koma", "first boot, fresh db", &[])).await.unwrap();
            store.insert(message(200, "koma", "still first boot", &[])).await.unwrap();
        }

        let store = Ledger::open(&path).await.unwrap().archive();
        if let Err(error) = store.insert(message(300, "koma", "second boot, reopened db", &[])).await {
            panic!("reopen insert failed: {error:#}");
        }

        let hits = store.search("reopened", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "search should see the post-reopen row");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fts_prefix_search_and_backfill() {
        let path = std::env::temp_dir().join("kurou-fts-search.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().archive();

        store.insert(message(100, "koma", "the crow keeps a database", &[])).await.unwrap();
        store.insert(message(200, "kurone", "unrelated chatter", &[])).await.unwrap();

        // prefix token: "data" finds "database"
        let hits = store.search("data", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "100");

        // multi-word is AND
        assert_eq!(store.search("crow database", 10).await.unwrap().len(), 1);
        assert_eq!(store.search("crow chatter", 10).await.unwrap().len(), 0);

        // fts operators arrive quoted, not parsed
        assert!(store.search("\"quoted\" AND (weird", 10).await.is_ok());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn backfill_indexes_rows_from_the_turso_era() {
        let path = std::env::temp_dir().join("kurou-fts-backfill.db");
        let _ = std::fs::remove_file(&path);

        {
            let store = Ledger::open(&path).await.unwrap().archive();
            store.insert(message(100, "koma", "written before the index existed", &[])).await.unwrap();
            // strip the fts table and triggers - this db now looks like one turso left behind
            let conn = crate::ledger::connect(&path).unwrap();
            conn.execute_batch(
                "drop trigger msg_fts_ai; drop trigger msg_fts_ad; drop trigger msg_fts_au; drop table msg_fts;",
            )
            .unwrap();
        }

        let store = Ledger::open(&path).await.unwrap().archive();
        let hits = store.search("index", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "pre-fts rows should be searchable after the backfill");

        let _ = std::fs::remove_file(&path);
    }
}
