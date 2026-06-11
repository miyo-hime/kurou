mod tools;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{Router, middleware, routing::get};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    tool_handler,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use serenity::model::id::GuildId;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthConfig;
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
    version = "0.3.0",
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

pub async fn run_http(config: Config) -> Result<()> {
    let token = config
        .discord_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .context("DISCORD_TOKEN is required (no token, no window)")?
        .to_string();
    let default_guild = config
        .discord_guild_id
        .as_deref()
        .map(parse_guild_id)
        .transpose()?;
    let bind_addr: std::net::SocketAddr = (config.host, config.port).into();
    let allowed_hosts = allowed_hosts(&config);
    let allowed_origins = config.allowed_origins.clone();
    let auth = Arc::new(AuthConfig::new(
        config.public_base_url.clone(),
        &config.auth_tokens,
    ));
    let cancellation = CancellationToken::new();

    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts.clone())
        .with_allowed_origins(allowed_origins.clone())
        .with_cancellation_token(cancellation.child_token());
    let service: StreamableHttpService<KurouServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(KurouServer::new(DiscordClient::new(&token), default_guild)),
            Default::default(),
            http_config,
        );

    let mcp_router = Router::new().nest_service("/mcp", service);
    let mcp_router = if auth.is_enabled() {
        let auth_for_middleware = auth.clone();
        tracing::info!(
            tokens = auth.token_count(),
            public_base_url = %config.public_base_url,
            "http bearer auth enabled"
        );
        mcp_router.layer(middleware::from_fn(move |request, next| {
            crate::auth::auth_middleware(auth_for_middleware.clone(), request, next)
        }))
    } else {
        tracing::warn!("http bearer auth is disabled because AUTH_TOKENS is empty");
        mcp_router
    };
    let metadata_router = if auth.is_enabled() {
        let access_token = auth
            .oauth_token(config.oauth_token_label.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "OAUTH_TOKEN_LABEL={} did not match any AUTH_TOKENS label",
                    config.oauth_token_label.as_deref().unwrap_or("")
                )
            })?;
        let oauth_store = crate::oauth::OAuthStore::new(access_token, config.public_base_url);
        tracing::info!(
            label = config
                .oauth_token_label
                .as_deref()
                .unwrap_or("<first-token>"),
            "oauth authorization-code shim enabled"
        );
        Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                get(crate::oauth::metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                get(crate::oauth::protected_resource),
            )
            .route(
                "/authorize",
                get(crate::oauth::authorize_get).post(crate::oauth::authorize_post),
            )
            .route("/token", axum::routing::post(crate::oauth::token))
            .with_state(oauth_store)
    } else {
        if config.oauth_token_label.is_some() {
            tracing::warn!(
                label = config.oauth_token_label.as_deref(),
                "OAUTH_TOKEN_LABEL is ignored because AUTH_TOKENS is empty"
            );
        }
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(crate::auth::protected_resource),
            )
            .with_state(auth)
    };
    let router = Router::new().merge(mcp_router).merge(metadata_router);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    tracing::info!(
        bind = %bind_addr,
        endpoint = %format!("http://{bind_addr}/mcp"),
        allowed_hosts = ?allowed_hosts,
        allowed_origins = ?allowed_origins,
        "kurou streamable http listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::warn!(%error, "failed to listen for shutdown signal");
            }
            cancellation.cancel();
        })
        .await?;

    Ok(())
}

fn parse_guild_id(raw: &str) -> Result<GuildId> {
    let id = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("DISCORD_GUILD_ID '{raw}' is not a valid snowflake"))?;
    Ok(GuildId::new(id))
}

fn allowed_hosts(config: &Config) -> Vec<String> {
    if !config.allowed_hosts.is_empty() {
        return config.allowed_hosts.clone();
    }

    let host = config.host.to_string();
    let bind_addr = std::net::SocketAddr::new(config.host, config.port).to_string();
    vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
        host,
        bind_addr,
    ]
}
