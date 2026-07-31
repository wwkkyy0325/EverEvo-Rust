//! EverEvo Web Agent — standalone MCP search service.
//!
//! Runs as a stdio MCP server, spawned by `everevo-mcp`'s stdio transport.
//! Provides web_search, web_fetch, and web_browse tools with anti-detection
//! browser automation.

mod browser;
mod captcha;
mod extract;
mod protect;
mod search;
mod server;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            std::env::var("EVEREVO_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!("everevo-webagent starting (MCP stdio)");
    let srv = server::Server::new();
    srv.run();
    tracing::info!("everevo-webagent exiting");
}
