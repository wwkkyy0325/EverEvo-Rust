//! Kernel initialization — bootstrap the plugin registry + tools.
//!
//! This is THE entry point for setting up the kernel at server startup.
//! It creates the PluginRegistry, registers bootstrap tools, and returns
//! all the pieces needed by the server runtime.
//!
//! ## Startup integrity check
//!
//! On every startup, the kernel computes SHA256 of its own binary and
//! compares it against a stored checksum (`data/kernel.checksum`).
//! If tampering is detected, a CRITICAL error is logged but the server
//! continues — the file system protections (write_file + sandbox denylist)
//! already prevent most tampering; this is a detection layer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use everevo_core::tool::ToolRegistry;

use crate::bootstrap;
use crate::plugin::registry::PluginRegistry;

/// The result of kernel initialization — everything the server needs.
pub struct KernelState {
    /// The fully-populated tool registry (bootstrap tools + plugins).
    pub tool_registry: ToolRegistry,
    /// The plugin registry for managing plugin versions and canary routing.
    pub plugin_registry: Arc<PluginRegistry>,
    /// Path to the plugin source directory (for agent self-modification).
    pub plugins_source_dir: PathBuf,
}

/// Initialize the kernel.
///
/// 1. Verifies kernel binary integrity (SHA256 self-check)
/// 2. Opens the plugin registry at `data_dir/plugins/`
/// 3. Registers bootstrap tools (shell, read_file, write_file, plugin_status, plugin_rollback)
/// 4. Returns a `KernelState` ready for server use
///
/// Plugins are NOT loaded at init — they are registered per-session via
/// `PluginRegistry::register_plugin_tools()` when a session starts.
pub async fn init(data_dir: impl Into<PathBuf>) -> Result<KernelState, String> {
    let data_dir: PathBuf = data_dir.into();

    // ── Kernel integrity self-check ──────────────────────────────────
    verify_kernel_integrity(&data_dir);

    let plugins_dir = data_dir.join("plugins");

    // Determine plugins source directory (relative to workspace root) BEFORE
    // registering bootstrap tools — PluginDev needs the source dir, while
    // PluginRegistry uses the runtime data/plugins dir for binary storage.
    let plugins_source_dir = find_plugins_source_dir()?;

    // Open the plugin registry (runtime: binary storage, version config,
    // checksums, canary routing — all live under data/plugins)
    let plugin_registry = PluginRegistry::open(&plugins_dir)
        .await
        .map_err(|e| format!("Failed to open plugin registry: {e}"))?;
    let plugin_registry = Arc::new(plugin_registry);

    // Create tool registry with bootstrap tools.
    // PluginDev receives the SOURCE directory so it can list/edit/build plugins.
    let mut tool_registry = ToolRegistry::new();
    bootstrap::register_all(
        &mut tool_registry,
        Some(Arc::clone(&plugin_registry)),
        Some(plugins_source_dir.clone()),
    );

    tracing::info!(
        bootstrap_tools = tool_registry.len(),
        plugins_dir = %plugins_dir.display(),
        "Kernel initialized"
    );

    Ok(KernelState {
        tool_registry,
        plugin_registry,
        plugins_source_dir,
    })
}

/// Verify the running kernel binary hasn't been tampered with.
///
/// On first run, stores the checksum in `data/kernel.checksum`.
/// On subsequent runs, compares and logs CRITICAL if mismatched.
///
/// This is a **detection** layer — tampering requires bypassing both
/// `write_file` kernel protection AND sandbox denylist, which should
/// prevent writes to kernel paths in the first place.
fn verify_kernel_integrity(data_dir: &Path) {
    let checksum_path = data_dir.join("kernel.checksum");

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Cannot get current exe path — skipping integrity check");
            return;
        }
    };

    let actual_hash = match crate::protection::sha256_file(&exe_path) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, exe = %exe_path.display(), "Cannot compute kernel checksum");
            return;
        }
    };

    match std::fs::read_to_string(&checksum_path) {
        Ok(stored) => {
            let stored = stored.trim();
            if stored != actual_hash {
                tracing::error!(
                    expected = %stored,
                    actual = %actual_hash,
                    exe = %exe_path.display(),
                    "🔴 KERNEL INTEGRITY FAILURE — binary has been modified! \
                     This should not be possible if kernel protection is active. \
                     The server will continue but self-repair is not guaranteed."
                );
            } else {
                tracing::info!(
                    hash = %&actual_hash[..16],
                    "Kernel integrity verified — checksum matches"
                );
            }
        }
        Err(_) => {
            // First run — store the checksum
            if let Some(parent) = checksum_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&checksum_path, &actual_hash) {
                tracing::warn!(error = %e, path = %checksum_path.display(), "Failed to write kernel checksum");
            } else {
                tracing::info!(
                    hash = %&actual_hash[..16],
                    path = %checksum_path.display(),
                    "Kernel checksum stored (first run)"
                );
            }
        }
    }
}

/// Find the plugins source directory by walking up from the current exe.
fn find_plugins_source_dir() -> Result<PathBuf, String> {
    // Try relative to the workspace root (development).
    // everevo-kernel is at crates/kernel/everevo-kernel → need 3 parents to reach root.
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()              // crates/kernel
        .and_then(|p| p.parent()) // crates
        .and_then(|p| p.parent()) // project root
        .map(|p| p.join("plugins"));

    if let Some(ref p) = dev_path {
        if p.join("Cargo.toml").exists() {
            return Ok(p.clone());
        }
    }

    // Try relative to the data dir (production)
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();

    let prod_path = exe_dir.join("plugins");
    if prod_path.join("Cargo.toml").exists() {
        return Ok(prod_path);
    }

    Err("Cannot find plugins source directory (looked for plugins/Cargo.toml)".into())
}
