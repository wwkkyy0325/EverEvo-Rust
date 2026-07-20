//! Built-in tool implementations and registry factory.
//!
//! The `Tool` trait and `ToolRegistry` live in `everevo-core` so any crate
//! can implement tools. This module provides the built-in implementations
//! and a convenience constructor that registers them all.

pub mod builtins;

use std::sync::Arc;

use everevo_core::tool::ToolRegistry;

/// Build a `ToolRegistry` with all built-in tools registered.
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
    registry
}
