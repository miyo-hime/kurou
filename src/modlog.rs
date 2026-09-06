use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct ModlogStore {
    path: Arc<PathBuf>,
    notifier: Option<Notifier>,
}

// the crow's diary going public: every ledger write mirrored as an embed, posted with
// the crow's own voice no matter whose hand acted. fire-and-forget by design - a
// discord hiccup must never block or fail the write it narrates.
#[derive(Clone, Debug)]
pub struct Notifier {
    pub client: crate::discord::DiscordClient,
    pub channel: serenity::model::id::ChannelId,
}

impl Notifier {
    fn post(&self, source: Source, row: NewModAction, ledger_id: i64, reverts: Option<i64>) {
        if matches!(row.action.as_str(), "join" | "leave") {
            return;
        }
        let notifier = self.clone();
        tokio::spawn(async move {
            let embed = build_embed(source, &row, ledger_id, reverts);
            if let Err(error) = notifier.client.send_embed(notifier.channel, embed).await {
                tracing::warn!(error = format!("{error:#}"), ledger_id, "failed to post modlog embed");
            }
        });
    }
}

fn build_embed(source: Source, row: &NewModAction, ledger_id: i64, reverts: Option<i64>) -> serenity::builder::CreateEmbed {
    let (emoji, color) = match row.action.as_str() {
        "ban" => ("🔨", 0xd83c3e),
        "unban" | "warn_revoke" | "timeout_remove" | "unlock" => ("🕊️", 0x57f287),
        "kick" => ("🥾", 0xe67e22),
        "timeout" => ("🤐", 0xe67e22),
        "warn" => ("⚠️", 0xfee75c),
        "delete" | "purge" => ("🧹", 0xe67e22),
        "role_add" | "role_remove" => ("🎭", 0x5865f2),
        "nickname" => ("🏷️", 0x5865f2),
        "lock" => ("🔒", 0xd83c3e),
        "slowmode" => ("🐌", 0x5865f2),
        "invite_delete" => ("🚪", 0xe67e22),
        _ => ("🐦‍⬛", 0x5865f2),
    };
    let mut embed = serenity::builder::CreateEmbed::new()
        .title(format!("{emoji} {}", row.action))
        .color(color)
        .timestamp(serenity::model::timestamp::Timestamp::now())
        .footer(serenity::builder::CreateEmbedFooter::new(match source {
            Source::Crow => format!("ledger #{ledger_id} · by the crow's hand"),
            Source::Observed => format!("ledger #{ledger_id} · witnessed"),
        }));
    if let (Some(id), name) = (&row.target_id, row.target_name.as_deref().unwrap_or("?")) {
        embed = embed.field("target", format!("{name} (<@{id}>)"), true);
    }
    if let Some(executor) = &row.executor_name {
        embed = embed.field("executor", executor.clone(), true);
    }
    if let Some(channel) = &row.channel_id {
        embed = embed.field("channel", format!("<#{channel}>"), true);
    }
    if let Some(reason) = &row.reason {
        embed = embed.field("reason", reason.clone(), false);
    }
    if let Some(intent) = &row.intent {
        embed = embed.field("intent", intent.clone(), false);
    }
    if let Some(metadata) = &row.metadata {
        embed = embed.field("details", format!("`{metadata}`"), false);
    }
    if let Some(original) = reverts {
        embed = embed.field("reverts", format!("ledger #{original}"), true);
    }
    embed
}

// the load-bearing column: who wrote the row. observed = the watcher saw someone
// else moderate; crow = one of our own hands did it on intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Observed,
    Crow,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Observed => "observed",
            Source::Crow => "crow",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NewModAction {
    pub action: String,
    pub guild_id: String,
    pub target_id: Option<String>,
    pub target_name: Option<String>,
    pub channel_id: Option<String>,
    pub executor_id: Option<String>,
    pub executor_name: Option<String>,
    pub reason: Option<String>,
    pub intent: Option<String>,
    pub metadata: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModAction {
    pub id: i64,
    pub action: String,
    pub source: String,
    pub guild_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverted_by: Option<i64>,
    pub created_at: String,
}

#[derive(Clone, Debug, Default)]
pub struct ModlogFilter {
    pub target_id: Option<String>,
    pub executor_id: Option<String>,
    pub action: Option<String>,
    pub source: Option<String>,
    pub channel_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

impl ModlogStore {
    pub fn new(path: Arc<PathBuf>) -> Self {
        Self { path, notifier: None }
    }

    pub fn with_notifier(mut self, notifier: Option<Notifier>) -> Self {
        self.notifier = notifier;
        self
    }

    pub async fn record(&self, source: Source, action: NewModAction) -> Result<i64> {
        let path = self.path.clone();
        let for_embed = action.clone();
        let ledger_id = tokio::task::spawn_blocking(move || -> Result<i64> {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            conn.execute(
                r#"
                insert into modlog (
                    action, source, guild_id, target_id, target_name, channel_id,
                    executor_id, executor_name, reason, intent, metadata, expires_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    action.action,
                    source.as_str(),
                    action.guild_id,
                    action.target_id,
                    action.target_name,
                    action.channel_id,
                    action.executor_id,
                    action.executor_name,
                    action.reason,
                    action.intent,
                    action.metadata,
                    action.expires_at,
                ],
            )
            .context("insert modlog row")?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .context("modlog record task")??;
        if let Some(notifier) = &self.notifier {
            notifier.post(source, for_embed, ledger_id, None);
        }
        Ok(ledger_id)
    }

    // undoing leaves two rows: the original gains reverted_by, the reversal is its own
    // entry. refuses to revert what doesn't exist, was already reverted, or is the
    // wrong kind of action.
    pub async fn revert(&self, original_id: i64, expected_action: &str, source: Source, action: NewModAction) -> Result<i64> {
        let path = self.path.clone();
        let expected = expected_action.to_string();
        let (reversal_id, filled) = tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            let tx = conn.unchecked_transaction().context("begin revert")?;
            struct Original { action: String, reverted_by: Option<i64>, target_id: Option<String>, target_name: Option<String> }
            let original = tx
                .query_row(
                    "select action, reverted_by, target_id, target_name from modlog where id = ?1",
                    params![original_id],
                    |row| Ok(Original { action: row.get(0)?, reverted_by: row.get(1)?, target_id: row.get(2)?, target_name: row.get(3)? }),
                )
                .optional()
                .context("look up original row")?;
            let Some(original) = original else {
                anyhow::bail!("ledger row {original_id} does not exist");
            };
            if original.action != expected {
                anyhow::bail!("ledger row {original_id} is a '{}', not a '{expected}'", original.action);
            }
            if let Some(reverted_by) = original.reverted_by {
                anyhow::bail!("ledger row {original_id} was already reverted by row {reverted_by}");
            }
            let mut action = action;
            action.target_id = action.target_id.or(original.target_id);
            action.target_name = action.target_name.or(original.target_name);
            tx.execute(
                r#"
                insert into modlog (
                    action, source, guild_id, target_id, target_name, channel_id,
                    executor_id, executor_name, reason, intent, metadata, expires_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    action.action,
                    source.as_str(),
                    action.guild_id,
                    action.target_id,
                    action.target_name,
                    action.channel_id,
                    action.executor_id,
                    action.executor_name,
                    action.reason,
                    action.intent,
                    action.metadata,
                    action.expires_at,
                ],
            )
            .context("insert reversal row")?;
            let reversal_id = tx.last_insert_rowid();
            tx.execute("update modlog set reverted_by = ?1 where id = ?2", params![reversal_id, original_id])
                .context("mark original reverted")?;
            tx.commit().context("commit revert")?;
            Ok((reversal_id, action))
        })
        .await
        .context("modlog revert task")??;
        if let Some(notifier) = &self.notifier {
            notifier.post(source, filled, reversal_id, Some(original_id));
        }
        Ok(reversal_id)
    }

    // the newest row of this kind against this user that nothing has undone yet -
    // how an unban finds the tempban it retires
    pub async fn latest_unreverted(&self, action: &str, target_id: &str) -> Result<Option<i64>> {
        let path = self.path.clone();
        let action = action.to_string();
        let target = target_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            conn.query_row(
                "select id from modlog where action = ?1 and target_id = ?2 and reverted_by is null order by id desc limit 1",
                params![action, target],
                |row| row.get(0),
            )
            .optional()
            .context("query latest unreverted")
        })
        .await
        .context("modlog latest task")?
    }

    pub async fn count_active(&self, action: &str, target_id: &str) -> Result<i64> {
        let path = self.path.clone();
        let action = action.to_string();
        let target = target_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            conn.query_row(
                "select count(*) from modlog where action = ?1 and target_id = ?2 and reverted_by is null",
                params![action, target],
                |row| row.get(0),
            )
            .context("count active rows")
        })
        .await
        .context("modlog count task")?
    }

    // the scheduler's worklist: crow actions carrying an unexpired expiry. the time
    // comparison happens in rust - text timestamps stay out of sql date math.
    pub async fn pending_expiries(&self) -> Result<Vec<ModAction>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            let mut stmt = conn
                .prepare(
                    r#"
                    select
                        id, action, source, guild_id, target_id, target_name, channel_id,
                        executor_id, executor_name, reason, intent, metadata, expires_at,
                        reverted_by, created_at
                    from modlog
                    where expires_at is not null and reverted_by is null
                      and action = 'ban' and source = 'crow'
                    "#,
                )
                .context("prepare pending expiries")?;
            let actions = stmt
                .query_map([], map_row)
                .context("query pending expiries")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read pending expiry rows")?;
            Ok(actions)
        })
        .await
        .context("modlog pending task")?
    }

    pub async fn query(&self, filter: ModlogFilter, limit: u8) -> Result<Vec<ModAction>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::ledger::connect(&path).context("modlog connect")?;
            let mut clauses: Vec<&str> = Vec::new();
            let mut binds: Vec<String> = Vec::new();
            let filters = [
                ("target_id = ?", filter.target_id),
                ("executor_id = ?", filter.executor_id),
                ("action = ?", filter.action),
                ("source = ?", filter.source),
                ("channel_id = ?", filter.channel_id),
                ("created_at >= ?", filter.since),
                ("created_at <= ?", filter.until),
            ];
            for (clause, value) in filters {
                if let Some(value) = value {
                    clauses.push(clause);
                    binds.push(value);
                }
            }
            let where_sql = if clauses.is_empty() { "1 = 1".to_string() } else { clauses.join(" and ") };
            let query = format!(
                r#"
                select
                    id, action, source, guild_id, target_id, target_name, channel_id,
                    executor_id, executor_name, reason, intent, metadata, expires_at,
                    reverted_by, created_at
                from modlog
                where {where_sql}
                order by id desc
                limit {limit}
                "#
            );

            let mut stmt = conn.prepare(&query).context("prepare modlog query")?;
            let actions = stmt
                .query_map(rusqlite::params_from_iter(binds), map_row)
                .context("query modlog")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read modlog rows")?;
            Ok(actions)
        })
        .await
        .context("modlog query task")?
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModAction> {
    Ok(ModAction {
        id: row.get(0)?,
        action: row.get(1)?,
        source: row.get(2)?,
        guild_id: row.get(3)?,
        target_id: row.get(4)?,
        target_name: row.get(5)?,
        channel_id: row.get(6)?,
        executor_id: row.get(7)?,
        executor_name: row.get(8)?,
        reason: row.get(9)?,
        intent: row.get(10)?,
        metadata: row.get(11)?,
        expires_at: row.get(12)?,
        reverted_by: row.get(13)?,
        created_at: row.get(14)?,
    })
}

pub(crate) const SCHEMA: &str = r#"
    create table if not exists modlog (
        id integer primary key autoincrement,
        action text not null,
        source text not null,
        guild_id text not null,
        target_id text,
        target_name text,
        channel_id text,
        executor_id text,
        executor_name text,
        reason text,
        intent text,
        metadata text,
        expires_at text,
        reverted_by integer,
        created_at text not null default current_timestamp
    );

    create index if not exists idx_modlog_target on modlog(target_id, id);
    create index if not exists idx_modlog_executor on modlog(executor_id, id);
    create index if not exists idx_modlog_pending_expiry on modlog(expires_at)
        where expires_at is not null and reverted_by is null;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn ban(target: &str) -> NewModAction {
        NewModAction {
            action: "ban".to_string(),
            guild_id: "guild".to_string(),
            target_id: Some(target.to_string()),
            target_name: Some("scammer".to_string()),
            executor_id: Some("kurone".to_string()),
            executor_name: Some("kurone".to_string()),
            reason: Some("crypto spam".to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn modlog_roundtrip_and_filters() {
        let path = std::env::temp_dir().join("kurou-modlog-roundtrip.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().modlog();

        let first = store.record(Source::Observed, ban("100")).await.unwrap();
        let warn = NewModAction {
            action: "warn".to_string(),
            guild_id: "guild".to_string(),
            target_id: Some("100".to_string()),
            executor_id: Some("koma".to_string()),
            intent: Some("koma: second strike for link spam".to_string()),
            ..Default::default()
        };
        let second = store.record(Source::Crow, warn).await.unwrap();
        store.record(Source::Observed, ban("200")).await.unwrap();
        assert!(second > first);

        let all = store.query(ModlogFilter::default(), 50).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].target_id.as_deref(), Some("200"), "newest first");

        let history = store.query(ModlogFilter { target_id: Some("100".to_string()), ..Default::default() }, 50).await.unwrap();
        assert_eq!(history.len(), 2, "observed and crow rows interleave");
        assert_eq!(history[0].source, "crow");
        assert_eq!(history[1].source, "observed");

        let crow_only = store.query(ModlogFilter { source: Some("crow".to_string()), ..Default::default() }, 50).await.unwrap();
        assert_eq!(crow_only.len(), 1);
        assert_eq!(crow_only[0].intent.as_deref(), Some("koma: second strike for link spam"));

        let bans = store.query(ModlogFilter { action: Some("ban".to_string()), ..Default::default() }, 1).await.unwrap();
        assert_eq!(bans.len(), 1, "limit applies");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn revert_links_rows_and_refuses_nonsense() {
        let path = std::env::temp_dir().join("kurou-modlog-revert.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().modlog();

        let warn = NewModAction {
            action: "warn".to_string(),
            guild_id: "guild".to_string(),
            target_id: Some("100".to_string()),
            target_name: Some("brat".to_string()),
            reason: Some("link spam".to_string()),
            ..Default::default()
        };
        let warn_id = store.record(Source::Crow, warn).await.unwrap();
        let revocation = NewModAction { action: "warn_revoke".to_string(), guild_id: "guild".to_string(), ..Default::default() };

        let missing = store.revert(999, "warn", Source::Crow, revocation.clone()).await.unwrap_err();
        assert!(missing.to_string().contains("does not exist"));
        let wrong_kind = store.revert(warn_id, "ban", Source::Crow, revocation.clone()).await.unwrap_err();
        assert!(wrong_kind.to_string().contains("not a 'ban'"));

        let reversal_id = store.revert(warn_id, "warn", Source::Crow, revocation.clone()).await.unwrap();
        let rows = store.query(ModlogFilter { target_id: Some("100".to_string()), ..Default::default() }, 10).await.unwrap();
        assert_eq!(rows.len(), 2, "reversal inherits the original's target");
        assert_eq!(rows[0].action, "warn_revoke");
        assert_eq!(rows[0].target_name.as_deref(), Some("brat"));
        assert_eq!(rows[1].reverted_by, Some(reversal_id));

        let twice = store.revert(warn_id, "warn", Source::Crow, revocation).await.unwrap_err();
        assert!(twice.to_string().contains("already reverted"));

        let _ = std::fs::remove_file(&path);
    }
}
