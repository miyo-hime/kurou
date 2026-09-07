use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::clock::house_time;
use crate::modlog::{ModAction, ModlogFilter};
use crate::server::KurouServer;
use crate::server::tools::common::{json_text, tool_error};

pub fn router() -> ToolRouter<KurouServer> {
    KurouServer::modlog_router()
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct CheckLedgerRequest {
    #[schemars(description = "only rows about this user (snowflake id)")]
    pub target_id: Option<String>,
    #[schemars(description = "only rows by this moderator (snowflake id)")]
    pub executor_id: Option<String>,
    #[schemars(description = "only this action, e.g. ban, unban, kick, timeout, warn, purge")]
    pub action: Option<String>,
    #[schemars(description = "'observed' (the watcher saw another moderator act) or 'crow' (a sister acted through kurou)")]
    pub source: Option<String>,
    #[schemars(description = "only rows in this channel (snowflake id) - purges, locks, message deletions")]
    pub channel_id: Option<String>,
    #[schemars(description = "only rows at or after this utc time, 'YYYY-MM-DD HH:MM:SS' (a 'YYYY-MM-DD' prefix works)")]
    pub since: Option<String>,
    #[schemars(description = "only rows at or before this utc time, same format as since")]
    pub until: Option<String>,
    #[schemars(description = "how many rows to return, 1-100, defaults to 25")]
    pub limit: Option<u8>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema, Serialize)]
pub struct UserHistoryRequest {
    #[schemars(description = "user snowflake id whose moderation record to read")]
    pub user_id: String,
    #[schemars(description = "how many rows to return, 1-100, defaults to 50")]
    pub limit: Option<u8>,
}

#[tool_router(router = modlog_router)]
impl KurouServer {
    #[tool(
        name = "check_ledger",
        description = "Query the moderation ledger: every mod action the crow witnessed or performed, filterable by target, executor, action, source and time range. Newest first."
    )]
    pub async fn check_ledger(
        &self,
        Parameters(CheckLedgerRequest { target_id, executor_id, action, source, channel_id, since, until, limit }): Parameters<CheckLedgerRequest>,
    ) -> Result<String, String> {
        // a bare date as until would sort before that day's own timestamps and exclude it
        let until = until.map(|until| if until.len() == 10 { format!("{until} 23:59:59") } else { until });
        let filter = ModlogFilter { target_id, executor_id, action, source, channel_id, since, until };
        let mut actions = self.modlog()?.query(filter, limit.unwrap_or(25).clamp(1, 100)).await.map_err(tool_error)?;
        render_times(&mut actions);
        json_text(&actions)
    }

    #[tool(
        name = "user_history",
        description = "One user's whole moderation record - everything that happened to them, observed and crow-issued actions interleaved, newest first."
    )]
    pub async fn user_history(
        &self,
        Parameters(UserHistoryRequest { user_id, limit }): Parameters<UserHistoryRequest>,
    ) -> Result<String, String> {
        let filter = ModlogFilter { target_id: Some(user_id), ..Default::default() };
        let mut actions = self.modlog()?.query(filter, limit.unwrap_or(50).clamp(1, 100)).await.map_err(tool_error)?;
        render_times(&mut actions);
        json_text(&actions)
    }
}

fn render_times(actions: &mut [ModAction]) {
    for action in actions {
        action.created_at = house_time(&action.created_at);
        action.expires_at = action.expires_at.as_deref().map(house_time);
    }
}

impl KurouServer {
    pub(crate) fn modlog(&self) -> Result<&crate::modlog::ModlogStore, String> {
        self.modlog_store
            .as_ref()
            .ok_or_else(|| "the mod ledger is closed: set MODLOG=true to arm the moderation layer".to_string())
    }
}
