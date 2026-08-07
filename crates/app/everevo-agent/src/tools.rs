//! Built-in tool implementations, audit hooks, and CLI registry factory.
//!
//! The `Tool` trait and `ToolRegistry` live in `everevo-core` so any crate
//! can implement tools. This module provides the built-in implementations
//! and a convenience constructor for CLI mode.
//!
//! For the full server-mode registry, see `orchestration::tools::assemble()`
//! in the `everevo-server` crate. The two registries should stay in sync —
//! any tool added to one should typically be added to the other.

pub mod audit_hook;
pub mod builtins;
pub mod reflect_gate;
pub mod review_gate;

use std::sync::Arc;

use everevo_core::tool::ToolRegistry;

/// Build a minimal `ToolRegistry` for CLI mode (7 of 11 tools).
///
/// For the full server-mode registry (Memory, TodoWrite, Task, Workflow, MCP
/// tools, etc.), see `orchestration::tools::assemble()` in the server crate.
///
/// Keep in sync: any tool registered here should also be in the server registry.
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
    registry.register(Arc::new(builtins::CompactTool::new()));
    registry.register(Arc::new(builtins::TeamTool::new()));
    registry.register(Arc::new(builtins::WorkflowRunnerTool::new()));
    let workspace = std::env::current_dir().unwrap_or_default();
    registry.register(Arc::new(builtins::CodeSearchTool::new(workspace.clone())));
    registry.register(Arc::new(builtins::CodeMapTool::new(workspace)));
    let skills_dir = std::path::PathBuf::from("data/skills");
    let skill_reg = Arc::new(crate::skill::SkillRegistry::load(&skills_dir)
        .unwrap_or_else(|_| crate::skill::SkillRegistry::empty()));
    registry.register(Arc::new(builtins::SkillTool::new(skill_reg)));
    registry
}
