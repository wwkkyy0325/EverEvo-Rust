//! Built-in tool implementations, audit hooks, and registry factory.
//!
//! The `Tool` trait and `ToolRegistry` live in `everevo-core` so any crate
//! can implement tools. This module provides the built-in implementations
//! and a convenience constructor that registers them all.

pub mod audit_hook;
pub mod builtins;

use std::sync::Arc;

use everevo_core::tool::ToolRegistry;

/// Build a minimal `ToolRegistry` for CLI mode (7 of 11 tools).
///
/// For the full 11-tool registry (Memory, TodoWrite, Task, Workflow added),
/// use `orchestration::tools::assemble()` in the server crate.
///
/// Pass `None` for optional backends — the corresponding tool is simply skipped.
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
        std::path::PathBuf::from("data"),
    )));
    registry.register(Arc::new(builtins::ExitPlanModeTool::new(
        Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        std::path::PathBuf::from("data"),
    )));
    registry.register(Arc::new(builtins::VerifyTool));
    registry.register(Arc::new(builtins::WebFetchTool));
    registry.register(Arc::new(builtins::CompactTool::new()));
    registry.register(Arc::new(builtins::TeamTool::new()));
    registry.register(Arc::new(builtins::WorkflowRunnerTool::new()));
    let workspace = std::env::current_dir().unwrap_or_default();
    registry.register(Arc::new(builtins::CodeSearchTool::new(workspace.clone())));
    registry.register(Arc::new(builtins::CodeMapTool::new(workspace)));
    registry.register(Arc::new(builtins::SkillTool::new(
        std::path::PathBuf::from("data/skills"),
    )));
    registry
}
