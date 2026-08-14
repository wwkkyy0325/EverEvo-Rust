//! Built-in tool implementations, audit hooks, and CLI registry factory.
//!
//! The `Tool` trait and `ToolRegistry` live in `everevo-core` so any crate
//! can implement tools. This module provides the built-in implementations
//! and a convenience constructor for CLI mode.
//!
//! All tool IMPLEMENTATIONS live in this crate (P1.1 tool-ownership refactor:
//! the server-layer tools — ask_user / problem_model / pipeline / sandbox /
//! web_search_delegate — moved here and depend on `session_store::SessionStore`
//! for session state). What remains split is the REGISTRY: `build_registry()`
//! is the lightweight CLI subset (`--chat`), while the HTTP/session registry
//! lives in `everevo-server::orchestration::tools::assemble()`. New tools are
//! implemented here and registered in whichever registry serves that run mode.

pub mod audit_hook;
pub mod builtins;
pub mod reflect_gate;
pub mod review_gate;
pub mod session_store;

use std::sync::Arc;

use everevo_core::tool::ToolRegistry;

/// The CLI-mode tool names (single source of truth for what `build_registry`
/// registers). `download`/`bootstrap_check` are conditional (only when the
/// corresponding provider is `Some`). A unit test below asserts the built
/// registry's names are exactly this set (minus the conditional ones), and a
/// server-side test asserts this set is a subset of the HTTP `assemble()`
/// registry — the drift guard that replaced the old "keep in sync" comment.
pub const CLI_REGISTRY_NAMES: &[&str] = &[
    "shell",
    "download",
    "bootstrap_check",
    "EnterPlanMode",
    "ExitPlanMode",
    "compact",
    "tool_cache_read",
    "team",
    "workflow_run",
    "code_search",
    "code_map",
    "Skill",
];

/// Build a minimal `ToolRegistry` for CLI mode (12 possible tools, 10 with
/// default `None` downloader/bootstrap).
///
/// For the full server-mode registry (Memory, TodoWrite, Task, Workflow, MCP
/// tools, etc.), see `orchestration::tools::assemble()` in the server crate.
pub fn build_registry(
    sandbox: Arc<dyn everevo_core::sandbox::SandboxProvider>,
    downloader: Option<Arc<everevo_downloader::Downloader>>,
    bootstrap: Option<Arc<everevo_bootstrap::Bootstrap>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(builtins::ShellTool::new(sandbox)));
    if let Some(dl) = downloader {
        registry.register(Arc::new(builtins::DownloadTool::new(dl)));
    }
    if let Some(bs) = bootstrap {
        registry.register(Arc::new(builtins::BootstrapTool::new(bs)));
    }
    // Stateless tools — always available
    registry.register(Arc::new(builtins::EnterPlanModeTool::new(
        Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        uuid::Uuid::nil(), // CLI is a single session
        std::path::PathBuf::from("data"),
    )));
    registry.register(Arc::new(builtins::ExitPlanModeTool::new(
        Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        uuid::Uuid::nil(), // CLI is a single session
        std::path::PathBuf::from("data"),
    )));
    registry.register(Arc::new(builtins::CompactTool::new()));
    registry.register(Arc::new(builtins::ToolCacheReadTool::new()));
    registry.register(Arc::new(builtins::TeamTool::new()));
    registry.register(Arc::new(builtins::WorkflowRunnerTool::new()));
    let workspace = std::env::current_dir().unwrap_or_default();
    registry.register(Arc::new(builtins::CodeSearchTool::new(workspace.clone())));
    registry.register(Arc::new(builtins::CodeMapTool::new(workspace)));
    let skills_dir = std::path::PathBuf::from("data/skills");
    let skill_reg = Arc::new(
        crate::skill::SkillRegistry::load(&skills_dir)
            .unwrap_or_else(|_| crate::skill::SkillRegistry::empty()),
    );
    registry.register(Arc::new(builtins::SkillTool::new(skill_reg)));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::tool::ToolRegistry;

    /// Drift guard (WS2, 2026-08-13): the CLI registry must register exactly
    /// `CLI_REGISTRY_NAMES` minus the conditional tools (`download`,
    /// `bootstrap_check`), which need providers the test can't easily build.
    #[test]
    fn cli_registry_matches_name_list() {
        let sandbox: Arc<dyn everevo_core::sandbox::SandboxProvider> = Arc::new(
            everevo_sandbox::TieredSandbox::new(everevo_sandbox::SandboxConfig::default()).unwrap(),
        );
        let registry: ToolRegistry = build_registry(sandbox, None, None);
        let mut names = registry.names();
        names.sort();
        let mut expected: Vec<&str> = CLI_REGISTRY_NAMES
            .iter()
            .copied()
            .filter(|n| *n != "download" && *n != "bootstrap_check")
            .collect();
        expected.sort();
        assert_eq!(
            names, expected,
            "CLI registry drifted from CLI_REGISTRY_NAMES — update the constant or build_registry"
        );
    }

    #[test]
    fn cli_name_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in CLI_REGISTRY_NAMES {
            assert!(
                seen.insert(*name),
                "duplicate tool name in CLI_REGISTRY_NAMES: {name}"
            );
        }
    }
}
