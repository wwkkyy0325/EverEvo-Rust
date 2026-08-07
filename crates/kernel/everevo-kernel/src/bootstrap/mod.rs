//! Bootstrap tools — compiled into kernel, never removable.
//!
//! These 6 tools guarantee self-repair capability even if the agent
//! breaks all 21 MCP plugins. They are registered directly into the
//! ToolRegistry at kernel init.
//!
//! ## Self-repair guarantee
//!
//! The agent can always:
//! 1. `shell` → git checkout broken plugin code + cargo build
//! 2. `read_file` / `write_file` → inspect and fix source
//! 3. `plugin_dev` → structured list/source/edit/build of plugin code
//! 4. `plugin_status` → diagnose which plugin is broken
//! 5. `plugin_rollback` → emergency rollback any plugin

use everevo_core::tool::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;

use crate::plugin::registry::PluginRegistry;

pub mod plugin_dev;
pub mod plugin_rollback;
pub mod plugin_status;
pub mod read_file;
pub mod shell;
pub mod write_file;

/// Register all bootstrap tools into a ToolRegistry.
/// Called once at kernel init. These cannot be removed.
///
/// Pass the PluginRegistry so `plugin_status` and `plugin_rollback`
/// can query and manage plugin versions.
/// Pass `plugins_dir` for `plugin_dev` source access (default: workspace plugins/).
pub fn register_all(
    registry: &mut ToolRegistry,
    plugin_registry: Option<Arc<PluginRegistry>>,
    plugins_dir: Option<PathBuf>,
) {
    let pd = plugins_dir.unwrap_or_else(|| PathBuf::from("plugins"));
    registry.register(Arc::new(shell::BootstrapShell));
    registry.register(Arc::new(read_file::BootstrapReadFile));
    registry.register(Arc::new(write_file::BootstrapWriteFile));
    registry.register(Arc::new(plugin_dev::PluginDev::new(pd)));
    registry.register(Arc::new(plugin_status::PluginStatus::new(
        plugin_registry.clone(),
    )));
    registry.register(Arc::new(plugin_rollback::PluginRollback::new(
        plugin_registry,
    )));
}
