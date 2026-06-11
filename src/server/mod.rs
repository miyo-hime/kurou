mod tools;

use anyhow::{Context, Result};
use rmcp::{
    ServerHandler, ServiceExt, handler::server::router::tool::ToolRouter, tool_handler,
    transport::stdio,
};
use serenity::model::id::GuildId;

use crate::config::Config;
use crate::discord::DiscordClient;

#[derive(Clone, Debug)]
pub struct KurouServer {
    pub(crate) client: DiscordClient,
    pub(crate) default_guild: Option<GuildId>,
    tool_router: ToolRouter<Self>,
}

impl KurouServer {
    pub fn new(client: DiscordClient, default_guild: Option<GuildId>) -> Self {
        Self {
            client,
            default_guild,
            tool_router: Self::tool_router(),
        }
    }

    fn tool_router() -> ToolRouter<Self> {
        ToolRouter::new()
            + tools::info::router()
            + tools::channels::router()
            + tools::messages::router()
            + tools::send::router()
            + tools::users::router()
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "kurou",
    version = "0.2.0",
    instructions = "a small window into a discord server. crow on the wire. get_server_info, list_channels, read_messages, send_message, get_user_id_by_name."
)]
impl ServerHandler for KurouServer {}

pub async fn run_stdio(config: Config) -> Result<()> {
    let token = config
        .discord_token
        .context("DISCORD_TOKEN is required (no token, no window)")?;
    let default_guild = config
        .discord_guild_id
        .as_deref()
        .map(parse_guild_id)
        .transpose()?;

    let client = DiscordClient::new(&token);
    let service = KurouServer::new(client, default_guild)
        .serve(stdio())
        .await?;
    tracing::info!("kurou running on stdio");
    service.waiting().await?;
    Ok(())
}

fn parse_guild_id(raw: &str) -> Result<GuildId> {
    let id = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("DISCORD_GUILD_ID '{raw}' is not a valid snowflake"))?;
    Ok(GuildId::new(id))
}
