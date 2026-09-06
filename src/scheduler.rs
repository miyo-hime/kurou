use anyhow::{Context as _, Result};
use serenity::model::id::{GuildId, UserId};
use serenity::model::timestamp::Timestamp;
use tokio::task::JoinHandle;

use crate::discord::DiscordClient;
use crate::modlog::{ModAction, ModlogStore, NewModAction, Source};

// the one place the crow acts with nobody in the loop - and it's mechanical on purpose:
// a timer undoing a decision already made, never a judgment.
pub fn spawn_scheduler(client: DiscordClient, modlog: ModlogStore) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match due_expiries(&modlog).await {
                Ok(due) => {
                    for row in due {
                        if let Err(error) = expire(&client, &modlog, &row).await {
                            tracing::error!(error = format!("{error:#}"), ledger_id = row.id, "failed to expire tempban");
                        }
                    }
                }
                Err(error) => tracing::error!(error = format!("{error:#}"), "scheduler could not read pending expiries"),
            }
        }
    })
}

async fn due_expiries(modlog: &ModlogStore) -> Result<Vec<ModAction>> {
    let now = Timestamp::now().unix_timestamp();
    let pending = modlog.pending_expiries().await?;
    Ok(pending
        .into_iter()
        .filter(|row| match row.expires_at.as_deref().map(Timestamp::parse) {
            Some(Ok(expiry)) => expiry.unix_timestamp() <= now,
            Some(Err(error)) => {
                // never silently permanent: a garbage expiry needs a human's eyes
                tracing::warn!(ledger_id = row.id, error = %error, "tempban has an unparseable expires_at and will never expire on its own");
                false
            }
            None => false,
        })
        .collect())
}

async fn expire(client: &DiscordClient, modlog: &ModlogStore, row: &ModAction) -> Result<()> {
    let guild = row.guild_id.parse::<u64>().map(GuildId::new).context("ledger row has a malformed guild id")?;
    let target = row
        .target_id
        .as_deref()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(UserId::new)
        .context("ledger row has no usable target id")?;

    let reason = format!("tempban expired (ledger row {})", row.id);
    if let Err(error) = client.unban(guild, target, Some(&reason)).await {
        // someone beat us to it - the ban is gone either way, so the reversal still gets written
        let already_gone = matches!(
            error.downcast_ref::<serenity::Error>(),
            Some(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(response))) if response.status_code.as_u16() == 404
        );
        if !already_gone {
            return Err(error);
        }
        tracing::info!(ledger_id = row.id, %target, "tempban target was already unbanned; recording the reversal anyway");
    }

    let reversal = NewModAction {
        action: "unban".to_string(),
        guild_id: row.guild_id.clone(),
        executor_name: Some("kurou".to_string()),
        intent: Some(format!("scheduler: tempban from ledger row {} expired", row.id)),
        ..Default::default()
    };
    let reversal_id = modlog.revert(row.id, "ban", Source::Crow, reversal).await?;
    tracing::info!(ledger_id = row.id, reversal_id, %target, "tempban expired and lifted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn tempban(expires_at: Option<String>) -> NewModAction {
        NewModAction {
            action: "ban".to_string(),
            guild_id: "guild".to_string(),
            target_id: Some("100".to_string()),
            expires_at,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn due_expiries_returns_only_ripe_unreverted_tempbans() {
        let path = std::env::temp_dir().join("kurou-scheduler-due.db");
        let _ = std::fs::remove_file(&path);
        let store = Ledger::open(&path).await.unwrap().modlog();

        let now = Timestamp::now().unix_timestamp();
        let stamp = |secs: i64| Timestamp::from_unix_timestamp(secs).unwrap().to_string();
        let ripe = store.record(Source::Crow, tempban(Some(stamp(now - 60)))).await.unwrap();
        store.record(Source::Crow, tempban(Some(stamp(now + 3600)))).await.unwrap();
        store.record(Source::Crow, tempban(None)).await.unwrap();

        let due = due_expiries(&store).await.unwrap();
        assert_eq!(due.len(), 1, "only the past-due tempban is ripe");
        assert_eq!(due[0].id, ripe);

        let reversal = NewModAction { action: "unban".to_string(), guild_id: "guild".to_string(), ..Default::default() };
        store.revert(ripe, "ban", Source::Crow, reversal).await.unwrap();
        assert!(due_expiries(&store).await.unwrap().is_empty(), "reverted rows leave the worklist");

        let _ = std::fs::remove_file(&path);
    }
}
