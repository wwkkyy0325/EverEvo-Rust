//! Tool trait and registry — the extension point for agent capabilities.
//!
//! Lives in `everevo-core` so ANY crate can implement `Tool` without depending on agent.
//! Built-in tools are in `everevo-agent::tools::builtins`, MCP tools in future crates.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::types::RiskLevel;
use crate::EverEvoError;

// ── Tool Trait ──────────────────────────────────────────────────────────

/// A tool callable by the LLM agent.
///
/// Implement this in any crate to add a new capability.
/// Register via `ToolRegistry::register()`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (used by LLM for function calling).
    fn name(&self) -> &str;

    /// Human-readable description (passed to LLM as tool description).
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters (LLM function calling format).
    fn parameters_schema(&self) -> serde_json::Value;

    /// Risk level determines which sandbox tier to use.
    fn risk_level(&self) -> RiskLevel;

    /// Execute the tool. Receives the sandbox for isolated operations.
    /// Execute the tool with the given parameters.
    ///
    /// `cancel` allows cooperative cancellation — tools should check
    /// `cancel.is_cancelled()` before long-running work and return
    /// early with an appropriate error if cancelled.
    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError>;
}

/// Result of executing a tool — returned to the LLM as context.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// Image attachments (e.g. from `browser_screenshot`). Empty for text-only
    /// tools. Carried in-memory to the LLM; not persisted to DB.
    pub images: Vec<crate::llm::ImageData>,
}

impl ToolOutput {
    /// Convenience: a text-only successful output (most common case).
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            images: Vec::new(),
        }
    }
}

// ── Tool Hooks (PreToolUse / PostToolUse) ───────────────────────────────

/// Hook for intercepting tool execution. Claude Code pattern.
///
/// Register hooks via `ToolRegistry::add_hook()`. Hooks run in registration order.
#[async_trait]
pub trait ToolHook: Send + Sync {
    /// Called before a tool executes. Return `Err` to block the tool call.
    /// The error message is passed back to the LLM as the tool result.
    async fn pre_execute(
        &self,
        _tool_name: &str,
        _params: &serde_json::Value,
    ) -> Result<(), EverEvoError> {
        Ok(())
    }

    /// Called after a tool executes (fires even on error).
    async fn post_execute(
        &self,
        _tool_name: &str,
        _params: &serde_json::Value,
        _result: &Result<ToolOutput, EverEvoError>,
    ) {
    }
}

// ── Registry ────────────────────────────────────────────────────────────

/// Registry of available tools, keyed by name.
///
/// Thread-safe (wrapped in `Arc` by the caller).
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    pub hooks: Vec<Arc<dyn ToolHook>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            hooks: Vec::new(),
        }
    }

    /// Register a tool. Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Remove tools that don't match the predicate.
    /// Used to filter write tools in plan mode.
    pub fn retain(&mut self, predicate: impl Fn(&Arc<dyn Tool>) -> bool) {
        self.tools.retain(|_, tool| predicate(tool));
    }

    /// Add a hook that fires before/after every tool execution.
    pub fn add_hook(&mut self, hook: Arc<dyn ToolHook>) {
        self.hooks.push(hook);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// All registered tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// How many tools are registered.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Build JSON Schema for all registered tools (LLM function calling).
    pub fn as_tool_schemas(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema(),
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EverEvoError;

    struct TestTool;
    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "A test tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn risk_level(&self) -> RiskLevel {
            RiskLevel::Low
        }
        async fn execute(
            &self,
            _params: serde_json::Value,
            _cancel: Option<&CancellationToken>,
        ) -> Result<ToolOutput, EverEvoError> {
            Ok(ToolOutput {
                content: "ok".into(),
                is_error: false,
                ..Default::default()
            })
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TestTool));
        assert!(reg.get("test").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_schemas() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TestTool));
        let schemas = reg.as_tool_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["function"]["name"], "test");
    }

    #[test]
    fn test_registry_len() {
        let mut reg = ToolRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(TestTool));
        assert_eq!(reg.len(), 1);
    }
}
