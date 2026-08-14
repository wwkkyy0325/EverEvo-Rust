//! web_search_local delegate — research via a native-server-side-web-search provider.
//!
//! When the main model is an OpenAI/llama-server provider, the `web_search_local`
//! MCP plugin hits a cn.bing/Sogou chain that frequently returns empty results
//! (anti-bot / GFW), so the agent loops on useless searches and burns its
//! wall-clock budget. This in-process replacement runs the whole research turn
//! through the first Anthropic-format provider (e.g. DeepSeek) whose API natively
//! executes `web_search_20250305` server-side and synthesizes an answer within a
//! single request — the local model reads the researched text. Registered with the
//! same name as the plugin tool, so `ToolRegistry::register` replaces it
//! (`research_search` from the plugin stays untouched).

use std::sync::Arc;

use crate::llm::HttpClient;
use async_trait::async_trait;
use everevo_core::llm::LlmMessage;
use everevo_core::tool::{Tool, ToolOutput, ToolRegistry};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Runs a native server-side web search through a delegate provider and returns
/// the synthesized, sourced answer as the tool result.
pub struct WebSearchDelegateTool {
    /// Anthropic-format provider (e.g. DeepSeek) that executes web search server-side.
    pub llm: Arc<HttpClient>,
}

#[async_trait]
impl Tool for WebSearchDelegateTool {
    fn name(&self) -> &str {
        "web_search_local"
    }

    fn description(&self) -> &str {
        "Research a query on the live web via a server-side search (executed by the API provider) \
         and return a synthesized answer with key facts and source titles. Use FIRST for any factual \
         question needing up-to-date or external information. Use short keyword queries."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "max_results": { "type": "integer", "description": "Maximum number of results to consider (default: 5)", "default": 5 }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let query = params["query"].as_str().unwrap_or("").trim().to_string();
        if query.is_empty() {
            return Err(EverEvoError::Tool {
                tool: self.name().into(),
                message: "Missing 'query' parameter".into(),
            });
        }

        // Audit MEDIUM (2026-08-13): `max_results` was declared in the schema
        // but never read — the sub-agent searched with no result bound. Honor it.
        let max_results = params["max_results"].as_u64().unwrap_or(5).clamp(1, 20) as usize;

        // The delegate provider's API executes the search server-side and
        // synthesizes the answer; run_to_string already handles the native
        // `server_tool_use` → `web_search_tool_result` flow and returns text.
        let prompt = format!(
            "Research the following question using the web_search tool. Run multiple searches if \
             needed, then synthesize the evidence into a direct answer with the key facts and the \
             source titles you relied on. Rely on at most {max_results} distinct results — \
             prefer the most authoritative.\n\nQuestion: {query}"
        );
        let messages = vec![LlmMessage::user(&prompt)];
        let registry = Arc::new(ToolRegistry::new());
        let cancel = cancel.cloned().unwrap_or_else(CancellationToken::new);
        let llm: Arc<dyn everevo_core::LlmProvider> = self.llm.clone();
        let result = crate::AgentRun::sub_agent(3)
            .run_to_string(llm, registry, messages, cancel)
            .await;
        // Audit MEDIUM (2026-08-13): the tool returned is_error:false even when
        // the research sub-agent errored or was cancelled — the main loop then
        // treated garbage as a successful search. Surface failures explicitly.
        let is_error = result.is_empty()
            || result.starts_with("Error:")
            || result.starts_with("Cancelled.")
            || result.starts_with("error:");
        Ok(ToolOutput {
            content: result,
            is_error,
            ..Default::default()
        })
    }
}
