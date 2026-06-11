use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt, handler::server::router::tool::ToolRouter, tool_handler,
    transport::stdio,
};

use crate::config::Config;

#[derive(Clone, Debug)]
pub struct KurouServer {
    tool_router: ToolRouter<Self>,
}

impl KurouServer {
    pub fn new() -> Self {
        Self {
            tool_router: ToolRouter::new(),
        }
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "kurou",
    version = "0.1.0",
    instructions = "a read-only window into a discord server. crow on the wire. no tools wired yet."
)]
impl ServerHandler for KurouServer {}

pub async fn run_stdio(_config: Config) -> Result<()> {
    let service = KurouServer::new().serve(stdio()).await?;
    tracing::info!("kurou running on stdio");
    service.waiting().await?;
    Ok(())
}
