//! In-process compact tool with focus-channel integration for AgentLoop autocompact.
//!
//! Complemented by MCP plugin `plugin-compact` which provides a stateless summarization
//! tool. This in-process version integrates with the compact_focus shared channel that
//! the AgentLoop reads for auto-compaction — a feature the MCP plugin cannot provide.
//! This in-process implementation is kept for backward compatibility.
//! New development should use the MCP plugin version.

//! Compact tool — manually trigger context compaction mid-session.
//!
//! Claude Code equivalent: the `/compact` slash command. When called, the
//! agent loop will trigger autocompact on the next turn, summarizing older
//! messages and freeing context budget for new work.
//!
//! The `focus` parameter is wired through to `autocompact()`: the shared
//! `compact_focus` mutex is written by this tool's `execute()` and read
//! (then cleared) by the agent loop's compaction path on the next turn.

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub struct CompactTool {
    /// Shared focus hint — written here, read by agent loop's autocompact.
    compact_focus: Option<Arc<Mutex<Option<String>>>>,
    /// Dreaming engine for pre-compaction memory flush (OpenClaw pattern).
    dreaming_engine: Option<Arc<crate::memory::engine::DreamingEngine>>,
}

impl CompactTool {
    pub fn new() -> Self {
        Self {
            compact_focus: None,
            dreaming_engine: None,
        }
    }

    /// Wire the compact focus channel between this tool and the agent loop.
    pub fn with_compact_focus(mut self, focus: Arc<Mutex<Option<String>>>) -> Self {
        self.compact_focus = Some(focus);
        self
    }

    /// Wire the dreaming engine for pre-compaction memory flush.
    pub fn with_dreaming_engine(
        mut self,
        engine: Arc<crate::memory::engine::DreamingEngine>,
    ) -> Self {
        self.dreaming_engine = Some(engine);
        self
    }
}

impl Default for CompactTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CompactTool {
    fn name(&self) -> &str {
        "compact"
    }

    fn description(&self) -> &str {
        "Manually trigger context compaction to free up space in the conversation. \
         Use when the conversation is getting long, the LLM seems to be losing \
         track of earlier context, or after an error about context being too long. \
         Compaction summarizes older messages and frees context budget. \
         Parameters: focus (optional string) — what topic to prioritize preserving."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "focus": {
                    "type": "string",
                    "description": "Optional: topic or context to prioritize in the summary"
                }
            },
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let focus = params["focus"].as_str().unwrap_or("");

        // Write focus hint to shared channel for autocompact to pick up
        if !focus.is_empty() {
            if let Some(ref cf) = self.compact_focus {
                *cf.lock().unwrap_or_else(|e| e.into_inner()) = Some(focus.to_string());
            }
        }

        // OpenClaw pattern: silent memory flush before compaction
        // Flushes dreaming message buffer to diary to prevent context loss
        if let Some(ref engine) = self.dreaming_engine {
            engine.flush_on_session_end().await;
        }

        let msg = if focus.is_empty() {
            "Compaction triggered. Memory flushed, older messages will be summarized \
             on the next turn. Continue your work."
                .into()
        } else {
            format!(
                "Compaction triggered (focus: '{focus}'). Memory flushed, agent will \
                 summarize older messages while preserving context about '{focus}'."
            )
        };
        Ok(ToolOutput {
            content: msg,
            is_error: false,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_tool_name_and_schema() {
        let tool = CompactTool::new();
        assert_eq!(tool.name(), "compact");
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("focus").is_some());
        assert_eq!(tool.risk_level(), RiskLevel::Low);
    }
}
