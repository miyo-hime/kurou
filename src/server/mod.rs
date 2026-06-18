mod tools;

use std::sync::Arc;

use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
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
use crate::config::{Config, GatewayMode};
use crate::discord::DiscordClient;
use crate::gateway::GatewayConfig;
use crate::layout::LayoutStore;
use crate::mentions::MentionStore;
use crate::uploads::UploadStore;
use crate::wall::event::{EnrichCache, WallFanout, WallMessage};
use crate::wall::{ClientPool, WallState};

#[derive(Clone, Debug)]
pub struct KurouServer {
    pub(crate) client: DiscordClient,
    pub(crate) readonly_client: Option<DiscordClient>,
    pub(crate) default_guild: Option<GuildId>,
    pub(crate) readonly_guilds: Vec<GuildId>,
    pub(crate) mention_store: Option<MentionStore>,
    pub(crate) upload_store: UploadStore,
    tool_router: ToolRouter<Self>,
}

impl KurouServer {
    pub fn new(
        client: DiscordClient,
        readonly_client: Option<DiscordClient>,
        default_guild: Option<GuildId>,
        readonly_guilds: Vec<GuildId>,
        mention_store: Option<MentionStore>,
        upload_store: UploadStore,
    ) -> Self {
        Self {
            client,
            readonly_client,
            default_guild,
            readonly_guilds,
            mention_store,
            upload_store,
            tool_router: Self::tool_router(),
        }
    }

    // guild is known: primary token for the primary guild, observer for a secondary.
    pub(crate) fn client_for_guild(&self, guild: GuildId) -> &DiscordClient {
        match &self.readonly_client {
            Some(observer) if Some(guild) != self.default_guild => observer,
            _ => &self.client,
        }
    }

    // only a channel id in hand. with no observer it's always primary; otherwise probe
    // once - the primary bot sees its own guild's channels and 403s on the secondaries.
    pub(crate) async fn client_for_channel(
        &self,
        channel: serenity::model::id::ChannelId,
    ) -> &DiscordClient {
        let Some(observer) = &self.readonly_client else {
            return &self.client;
        };
        match self.client.channel_guild(channel).await {
            Ok(guild) if guild == self.default_guild => &self.client,
            _ => observer,
        }
    }

    fn tool_router() -> ToolRouter<Self> {
        ToolRouter::new()
            + tools::info::router()
            + tools::channels::router()
            + tools::messages::router()
            + tools::scan::router()
            + tools::mentions::router()
            + tools::send::router()
            + tools::users::router()
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "kurou",
    version = "0.7.0",
    instructions = "a small window into a discord server. crow on the wire. reads: list_servers, get_server_info, list_channels, list_threads, read_messages (anchor with around/before/after), get_message, get_pinned, scan_channel (deep author/mention/text sweep). voice: send_message, get_user_id_by_name. mentions: check_mentions, mark_mentions_seen. read-only secondary guilds ride a separate observer bot, routed for you."
)]
impl ServerHandler for KurouServer {}

// uploads are meant to be claimed seconds later by send_message. ten minutes is
// generous slack, not a parking lot.
const UPLOAD_TTL: Duration = Duration::from_secs(600);

pub async fn run_stdio(config: Config) -> Result<()> {
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
    let readonly_guilds = read_guild_allowlist(&config, default_guild)?;
    let readonly_client = build_readonly_client(&config, &readonly_guilds)?;

    let mention_store = mention_store(&config).await?;
    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            fanout: None,
        },
    );

    let client = DiscordClient::new(&token);
    let upload_store = UploadStore::new(UPLOAD_TTL);
    let service = KurouServer::new(
        client,
        readonly_client,
        default_guild,
        readonly_guilds,
        mention_store,
        upload_store,
    )
    .serve(stdio())
    .await?;
    tracing::info!("kurou running on stdio");
    service.waiting().await?;
    if let Some(gateway) = gateway {
        gateway.abort();
    }
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
    let readonly_guilds = read_guild_allowlist(&config, default_guild)?;
    let readonly_client = build_readonly_client(&config, &readonly_guilds)?;
    let bind_addr: std::net::SocketAddr = (config.host, config.port).into();
    let mention_store = mention_store(&config).await?;

    // the wall's plumbing: one enrichment cache shared by every gateway and the backfill
    // path, one broadcast both gateways pour into and every browser drinks from.
    let wall_enabled = config.wall;
    let enrich_cache = Arc::new(EnrichCache::new());
    let (wall_tx, _wall_rx) = tokio::sync::broadcast::channel::<Arc<WallMessage>>(256);

    let primary_fanout = wall_enabled.then(|| WallFanout {
        client: DiscordClient::new(&token),
        cache: enrich_cache.clone(),
        tx: wall_tx.clone(),
    });

    let gateway = crate::gateway::spawn_gateway(
        token.clone(),
        GatewayConfig {
            mode: config.gateway_mode,
            default_guild,
            mention_keywords: config.mention_keywords.clone(),
            mention_store: mention_store.clone(),
            fanout: primary_fanout,
        },
    );

    // the secondaries only go live through the observer's own gateway - a separate
    // invisible socket that watches and broadcasts but records nothing, hears nothing back.
    let observer_token = config
        .readonly_discord_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let observer_gateway = match (wall_enabled, observer_token, &readonly_client) {
        (true, Some(observer_token), Some(observer_client)) => crate::gateway::spawn_gateway(
            observer_token,
            GatewayConfig {
                mode: GatewayMode::Presence,
                default_guild: None,
                mention_keywords: Vec::new(),
                mention_store: None,
                fanout: Some(WallFanout {
                    client: observer_client.clone(),
                    cache: enrich_cache.clone(),
                    tx: wall_tx.clone(),
                }),
            },
        ),
        _ => None,
    };

    let wall_state = if wall_enabled {
        crate::ledger::initialize(config.ledger_path()).await?;
        Some(WallState {
            tx: wall_tx.clone(),
            cache: enrich_cache.clone(),
            pool: ClientPool {
                client: DiscordClient::new(&token),
                readonly_client: readonly_client.clone(),
                default_guild,
                readonly_guilds: readonly_guilds.clone(),
            },
            layout: LayoutStore::new(config.ledger_path()),
        })
    } else {
        None
    };

    let allowed_hosts = allowed_hosts(&config);
    let allowed_origins = config.allowed_origins.clone();
    let auth = Arc::new(AuthConfig::new(
        config.public_base_url.clone(),
        &config.auth_tokens,
    ));
    let cancellation = CancellationToken::new();

    let upload_store = UploadStore::new(UPLOAD_TTL);
    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts.clone())
        .with_allowed_origins(allowed_origins.clone())
        .with_cancellation_token(cancellation.child_token());
    let factory_store = upload_store.clone();
    let factory_readonly = readonly_guilds.clone();
    let factory_readonly_client = readonly_client.clone();
    let service: StreamableHttpService<KurouServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(KurouServer::new(
                    DiscordClient::new(&token),
                    factory_readonly_client.clone(),
                    default_guild,
                    factory_readonly.clone(),
                    mention_store.clone(),
                    factory_store.clone(),
                ))
            },
            Default::default(),
            http_config,
        );

    let upload_route = Router::new()
        .route("/upload", post(crate::uploads::upload_handler))
        .layer(DefaultBodyLimit::max(crate::uploads::MAX_UPLOAD_BYTES))
        .with_state(upload_store);

    let mcp_router = Router::new()
        .nest_service("/mcp", service)
        .merge(upload_route);
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
    let mut router = Router::new().merge(mcp_router).merge(metadata_router);
    if let Some(wall_state) = wall_state {
        // no bearer layer here on purpose - the wall trusts nginx's authelia forward-auth
        // and binds where only the proxy can reach it. the crow guards nothing itself.
        tracing::info!("nightwatch wall mounted at /wall");
        router = router.merge(crate::wall::router(wall_state));
    }
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

    if let Some(gateway) = gateway {
        gateway.abort();
    }
    if let Some(observer_gateway) = observer_gateway {
        observer_gateway.abort();
    }

    Ok(())
}

fn parse_guild_id(raw: &str) -> Result<GuildId> {
    let id = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("DISCORD_GUILD_ID '{raw}' is not a valid snowflake"))?;
    Ok(GuildId::new(id))
}

fn read_guild_allowlist(config: &Config, default_guild: Option<GuildId>) -> Result<Vec<GuildId>> {
    let guilds = config
        .readonly_guilds
        .iter()
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| {
            raw.trim()
                .parse::<u64>()
                .map(GuildId::new)
                .with_context(|| format!("READONLY_GUILDS '{raw}' is not a valid snowflake"))
        })
        .collect::<Result<Vec<_>>>()?;
    if !guilds.is_empty() && default_guild.is_none() {
        anyhow::bail!("READONLY_GUILDS is set but DISCORD_GUILD_ID (the primary, the only place send_message may post) is not");
    }
    Ok(guilds)
}

fn build_readonly_client(config: &Config, readonly_guilds: &[GuildId]) -> Result<Option<DiscordClient>> {
    let token = config
        .readonly_discord_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    match (readonly_guilds.is_empty(), token) {
        (true, _) => Ok(None),
        (false, Some(token)) => Ok(Some(DiscordClient::new(token))),
        (false, None) => {
            anyhow::bail!("READONLY_GUILDS is set but READONLY_DISCORD_TOKEN (the observer bot) is not")
        }
    }
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

async fn mention_store(config: &Config) -> Result<Option<MentionStore>> {
    if config.gateway_mode != GatewayMode::Mentions {
        return Ok(None);
    }

    let path = config.ledger_path();
    crate::ledger::initialize(path.clone()).await?;
    tracing::info!(path = %path.display(), "ledger initialized");
    Ok(Some(MentionStore::new(path)))
}
