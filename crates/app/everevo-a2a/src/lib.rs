//! EverEvo A2A (Agent-to-Agent) protocol layer.
//!
//! Implements the [A2A specification v0.3.0](https://google-a2a.github.io/A2A/specification/)
//! for agent interoperability.

pub mod card;
pub mod error;
pub mod executor;
pub mod middleware;
pub mod router;
pub mod state;
pub mod types;

use std::sync::Arc;

use everevo_agent::llm::HttpClient;
use everevo_core::tool::ToolRegistry;

use crate::card::AgentCardBuilder;
use crate::executor::{A2aAgentExecutor, EverEvoExecutor};
use crate::middleware::A2aAuthConfig;
use crate::router::{a2a_router, A2aState};

/// Configuration for the A2A gateway.
#[derive(Clone)]
pub struct A2aGatewayConfig {
    pub base_url: String,
    pub max_turns: usize,
    pub enable_auth: bool,
    pub jwt_secret: Option<String>,
    pub api_keys: Vec<String>,
    pub max_body_bytes: usize,
}

impl Default for A2aGatewayConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:3000".into(),
            max_turns: 50,
            enable_auth: false,
            jwt_secret: None,
            api_keys: vec![],
            max_body_bytes: 1_048_576,
        }
    }
}

/// The A2A gateway — pre-built router ready to merge.
pub struct A2aGateway {
    router: axum::Router<()>,
}

impl A2aGateway {
    /// Create an A2A gateway with the production executor.
    pub fn new(
        llm: Arc<HttpClient>,
        tools: Arc<ToolRegistry>,
        config: A2aGatewayConfig,
    ) -> Self {
        let executor = Arc::new(EverEvoExecutor::new(llm, tools, config.max_turns));
        let card = AgentCardBuilder::new(&config.base_url).build();
        let state = Arc::new(A2aState::new(executor, card, config.max_turns));

        let auth_config = if config.enable_auth {
            A2aAuthConfig::production(
                config.jwt_secret.unwrap_or_default(),
                config.api_keys,
            )
        } else {
            A2aAuthConfig::dev_mode()
        };

        let router = a2a_router(state)
            .layer(middleware::body_limit_layer(config.max_body_bytes))
            .layer(middleware::AuthLayer::new(auth_config));

        Self { router }
    }

    /// Create a gateway with a custom executor (for testing).
    pub fn with_executor(
        executor: Arc<dyn A2aAgentExecutor>,
        config: A2aGatewayConfig,
    ) -> Self {
        let card = AgentCardBuilder::new(&config.base_url).build();
        let state = Arc::new(A2aState::new(executor, card, config.max_turns));

        let router = a2a_router(state)
            .layer(middleware::body_limit_layer(config.max_body_bytes))
            .layer(middleware::AuthLayer::new(A2aAuthConfig::dev_mode()));

        Self { router }
    }

    /// Return the pre-built A2A router — `Router<()>` merges with any app.
    pub fn router(&self) -> axum::Router<()> {
        self.router.clone()
    }
}
