//! Plugin Dev — inspect and modify MCP plugin source code.
//!
//! Gives the agent structured access to its own editable (non-kernel) code.
//! All plugins live under `plugins/` as independent MCP binaries — they are
//! safe to modify because the kernel bootstrap tools guarantee recovery.
//!
//! ## Actions
//!
//! | Action | Description |
//! |--------|-------------|
//! | `list` | List all plugins with metadata (dir, tools, source files, status) |
//! | `source` | Read a plugin's full source code (main.rs + Cargo.toml) |
//! | `edit` | Write/modify a plugin's source file |
//! | `build` | Run `cargo build --release` for a specific plugin |
#![allow(clippy::disallowed_methods)] // kernel privilege: cargo build plugin binaries
use std::path::PathBuf;
use std::path::Path;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use crate::protection;

pub struct PluginDev {
    plugins_dir: PathBuf,
}

impl PluginDev {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self { plugins_dir }
    }

    /// Scan the plugins directory and collect metadata for each plugin.
    fn scan_plugins(&self) -> Vec<PluginInfo> {
        let mut plugins = Vec::new();
        for category in &["tools", "stages", "hooks"] {
            let dir = self.plugins_dir.join(category);
            if !dir.exists() { continue; }
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() { continue; }
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let main_rs = path.join("src").join("main.rs");
                    let cargo_toml = path.join("Cargo.toml");
                    if !main_rs.exists() || !cargo_toml.exists() { continue; }
                    // Try to parse tool names from the source
                    let tools = Self::extract_tool_names(&main_rs);
                    let main_size = std::fs::metadata(&main_rs).map(|m| m.len()).unwrap_or(0);
                    let binary_name = Self::infer_binary_name(category, &name);
                    plugins.push(PluginInfo {
                        category: category.to_string(),
                        name,
                        tools,
                        main_size,
                        binary_name,
                    });
                }
            }
        }
        plugins.sort_by(|a, b| a.category.cmp(&b.category).then(a.name.cmp(&b.name)));
        plugins
    }

    /// Quick scan of source for tool/prompt names (pattern: "name": "tool_name").
    fn extract_tool_names(main_rs: &PathBuf) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(content) = std::fs::read_to_string(main_rs) {
            for line in content.lines() {
                if let Some(start) = line.find("\"name\":") {
                    let rest = &line[start + 7..];
                    if let Some(quoted) = rest.trim().strip_prefix('"') {
                        if let Some(end) = quoted.find('"') {
                            let name = &quoted[..end];
                            if name != "initialize" && name != "notifications/initialized"
                                && name != "tools/list" && name != "tools/call"
                                && name != "ping" && name != "prompts/list" && name != "prompts/get"
                            {
                                names.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        names
    }

    fn infer_binary_name(category: &str, name: &str) -> String {
        match category {
            "stages" => format!("plugin-stages-{}.exe", name),
            "hooks" => format!("plugin-hooks-{}.exe", name),
            _ => format!("plugin-{}.exe", name),
        }
    }

    fn source_file(&self, category: &str, name: &str, file: &str) -> Option<PathBuf> {
        let path = self.plugins_dir.join(category).join(name).join(file);
        if path.exists() { Some(path) } else { None }
    }

    /// Read a plugin's source. Returns (main_rs, cargo_toml).
    fn read_source(&self, category: &str, name: &str) -> Result<(String, String), String> {
        let main_rs_path = self
            .source_file(category, name, "src/main.rs")
            .ok_or_else(|| format!("Plugin '{name}' main.rs not found in plugins/{category}/"))?;
        let cargo_toml_path = self
            .source_file(category, name, "Cargo.toml")
            .ok_or_else(|| format!("Plugin '{name}' Cargo.toml not found"))?;

        let main_rs = std::fs::read_to_string(&main_rs_path)
            .map_err(|e| format!("Read main.rs: {e}"))?;
        let cargo_toml = std::fs::read_to_string(&cargo_toml_path)
            .map_err(|e| format!("Read Cargo.toml: {e}"))?;
        Ok((main_rs, cargo_toml))
    }

    /// Write to a plugin's source file.
    /// Refuses to write outside the plugin directory (no path traversal)
    /// and blocks writes to kernel-protected paths.
    fn write_source(
        &self,
        category: &str,
        name: &str,
        file: &str,
        content: &str,
    ) -> Result<(), String> {
        // ── Validate category ─────────────────────────────────────────
        validate_plugin_category(category)?;
        validate_plugin_name(name)?;

        // ── Validate file name (no traversal) ─────────────────────────
        let file_path = Path::new(file);
        if file_path.components().count() != 1 {
            return Err(format!(
                "Invalid file name '{file}': must be a simple filename (main.rs, Cargo.toml, etc.), not a path"
            ));
        }
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file);
        if file_name.is_empty() || file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
            return Err(format!("Invalid file name: '{file}'"));
        }

        let path = self
            .source_file(category, name, file)
            .or_else(|| {
                // Allow creating new files too, but only within the plugin dir
                let p = self.plugins_dir.join(category).join(name).join(file_name);
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                Some(p)
            })
            .ok_or_else(|| format!("Cannot resolve path for {file} in plugin '{name}'"))?;

        // ── Kernel self-protection chokepoint ─────────────────────────
        let path_str = path.display().to_string();
        if protection::is_kernel_protected(&path_str) {
            return Err(format!(
                "BLOCKED: '{}' is in a kernel-protected area. Only plugin files under plugins/ are editable.",
                path_str
            ));
        }

        // ── Verify the resolved path stays within plugins/ dir ────────
        let canonical_plugins_dir = self.plugins_dir.canonicalize().unwrap_or_else(|_| self.plugins_dir.clone());
        // If we can't canonicalize (file doesn't exist yet), check the parent
        if let Ok(canonical) = path.canonicalize() {
            if !canonical.starts_with(&canonical_plugins_dir) {
                return Err(format!(
                    "BLOCKED: '{}' resolves outside the plugins directory",
                    path_str
                ));
            }
        } else if let Some(parent) = path.parent() {
            if let Ok(canonical_parent) = parent.canonicalize() {
                if !canonical_parent.starts_with(&canonical_plugins_dir) {
                    return Err(format!(
                        "BLOCKED: '{}' resolves outside the plugins directory",
                        path_str
                    ));
                }
            }
        }

        std::fs::write(&path, content).map_err(|e| format!("Write {file}: {e}"))
    }

    /// Build a plugin binary.
    /// Only builds `plugin-*` packages — refuses to build kernel crates.
    /// Uses spawn + concurrent read + timeout (NOT cmd.output()) to prevent
    /// OOM and orphan processes.
    fn build_plugin(&self, category: &str, name: &str) -> Result<String, String> {
        validate_plugin_category(category)?;
        validate_plugin_name(name)?;

        let pkg_name = Self::infer_binary_name(category, name).replace(".exe", "");

        // ── Kernel protection: only build plugin packages ─────────────
        if protection::is_kernel_package(&pkg_name) {
            return Err(format!(
                "BLOCKED: '{pkg_name}' is a kernel crate. Kernel code is immutable.\n\
                 Only plugin-* packages under plugins/ can be built."
            ));
        }
        if !protection::is_plugin_package(&pkg_name) {
            return Err(format!(
                "BLOCKED: '{pkg_name}' does not match plugin-* naming convention.\n\
                 Only plugin packages (plugin-tools-*, plugin-hooks-*, plugin-stages-*) can be built via plugin_dev."
            ));
        }

        let workspace_dir = self.plugins_dir.parent().unwrap_or(&self.plugins_dir);

        const MAX_OUTPUT_BYTES: usize = 500_000;
        const BUILD_TIMEOUT_SECS: u64 = 180;

        let mut child = std::process::Command::new("cargo")
            .args(["build", "--release", "-p", &pkg_name])
            .current_dir(workspace_dir)
            .env("CARGO_NET_OFFLINE", "true") // no network for plugin builds
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start cargo: {e}"))?;

        // Read both stdout and stderr concurrently using threads
        // to prevent pipe-buffer deadlock (std::process pipes are small).
        use std::io::Read;
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let (tx_out, rx_out) = std::sync::mpsc::channel();
        let (tx_err, rx_err) = std::sync::mpsc::channel();

        if let Some(reader) = stdout_handle {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = reader
                    .take(MAX_OUTPUT_BYTES as u64)
                    .read_to_end(&mut buf);
                let _ = tx_out.send(String::from_utf8_lossy(&buf).to_string());
            });
        } else {
            let _ = tx_out.send(String::new());
        }

        if let Some(reader) = stderr_handle {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = reader
                    .take(MAX_OUTPUT_BYTES as u64)
                    .read_to_end(&mut buf);
                let _ = tx_err.send(String::from_utf8_lossy(&buf).to_string());
            });
        } else {
            let _ = tx_err.send(String::new());
        }

        // Wait for the build process with timeout
        let (tx_wait, rx_wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = child.wait();
            let _ = tx_wait.send(result);
        });

        let status = match rx_wait.recv_timeout(std::time::Duration::from_secs(BUILD_TIMEOUT_SECS)) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("Build process error: {e}")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // child was moved into the thread — we can't kill it from here.
                // The thread will run to completion; we just report the timeout.
                return Err(format!(
                    "Build timed out after {BUILD_TIMEOUT_SECS}s. The cargo process will terminate naturally."
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Build process crashed".to_string());
            }
        };

        let stdout_str = rx_out.recv().unwrap_or_default();
        let stderr_str = rx_err.recv().unwrap_or_default();
        let combined = format!("{stdout_str}\n{stderr_str}");

        if status.success() {
            let binary = workspace_dir
                .join("target")
                .join("release")
                .join(Self::infer_binary_name(category, name));
            if binary.exists() {
                let size = binary.metadata().map(|m| m.len()).unwrap_or(0);
                Ok(format!(
                    "Build succeeded.\n  Binary: {}\n  Size: {} bytes\n\n{}",
                    binary.display(),
                    size,
                    combined.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
                ))
            } else {
                Ok(format!("Build succeeded but binary not found at expected path.\n\n{combined}"))
            }
        } else {
            Err(format!(
                "Build failed (exit {}).\n\n{}",
                status.code().unwrap_or(-1),
                combined
            ))
        }
    }
}

struct PluginInfo {
    category: String,
    name: String,
    tools: Vec<String>,
    main_size: u64,
    binary_name: String,
}

// ── Validation ─────────────────────────────────────────────────────────────

/// Validate that a plugin category is one of the allowed values.
fn validate_plugin_category(category: &str) -> Result<(), String> {
    match category {
        "tools" | "stages" | "hooks" => Ok(()),
        other => Err(format!(
            "Invalid category '{other}'. Must be one of: tools, stages, hooks."
        )),
    }
}

/// Validate that a plugin name doesn't contain path traversal or
/// kernel-protected patterns. Names must be simple directory names.
fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Plugin name cannot be empty.".to_string());
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(format!(
            "Invalid plugin name '{name}': must not contain path separators or traversal."
        ));
    }
    if name.len() > 64 {
        return Err(format!(
            "Plugin name '{name}' is too long (max 64 characters)."
        ));
    }
    // Only allow alphanumeric, dash, underscore
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "Invalid plugin name '{name}': only alphanumeric, dash, and underscore allowed."
        ));
    }
    Ok(())
}

#[async_trait]
impl Tool for PluginDev {
    fn name(&self) -> &str { "plugin_dev" }
    fn description(&self) -> &str {
        "Inspect and modify MCP plugin source code (non-kernel, safe to edit). \
         Actions: 'list' all plugins with metadata, 'source' to read a plugin's code, \
         'edit' to write source changes, 'build' to compile a plugin. \
         Combined with plugin_status and plugin_rollback for full self-modification."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "source", "edit", "build"],
                    "description": "Action: 'list' plugins, 'source' to read code, 'edit' to write changes, 'build' to compile"
                },
                "category": {
                    "type": "string",
                    "enum": ["tools", "stages", "hooks"],
                    "description": "Plugin category (required for source/edit/build)"
                },
                "name": {
                    "type": "string",
                    "description": "Plugin name (required for source/edit/build, e.g. 'web_search', 'best_practices')"
                },
                "file": {
                    "type": "string",
                    "enum": ["main.rs", "Cargo.toml"],
                    "description": "Which source file to read/edit (default: 'main.rs')"
                },
                "content": {
                    "type": "string",
                    "description": "New file content (required for 'edit' action)"
                }
            },
            "required": ["action"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let action = params["action"].as_str().unwrap_or("list");

        match action {
            "list" => {
                let plugins = self.scan_plugins();
                if plugins.is_empty() {
                    return Ok(ToolOutput::text(
                        "No plugins found. Ensure plugins/ directory exists with tools/, stages/, hooks/ subdirectories."
                    ));
                }
                let mut lines = vec![format!(
                    "{} plugins across 3 categories:\n",
                    plugins.len()
                )];
                let mut current_cat = String::new();
                for p in &plugins {
                    if p.category != current_cat {
                        current_cat = p.category.clone();
                        lines.push(format!("\n## plugins/{}/", current_cat));
                    }
                    let tool_list = if p.tools.is_empty() {
                        "(no tools detected)".into()
                    } else {
                        p.tools.join(", ")
                    };
                    lines.push(format!(
                        "  {:<25} {:>6} bytes  → {}  [{}]",
                        p.name, p.main_size, tool_list, p.binary_name
                    ));
                }
                lines.push(
                    "\nUse plugin_dev(action='source', category='...', name='...') to read a plugin's full source code."
                        .to_string(),
                );
                Ok(ToolOutput::text(lines.join("\n")))
            }

            "source" => {
                let category = params["category"].as_str().unwrap_or("tools");
                let name = params["name"].as_str().unwrap_or("");
                let file = params["file"].as_str().unwrap_or("main.rs");
                if name.is_empty() {
                    return Ok(ToolOutput { content: "Provide 'name' and 'category' for action='source'.".into(), is_error: true, ..Default::default() });
                }
                if file == "Cargo.toml" {
                    // Return only Cargo.toml
                    let path = self.source_file(category, name, file)
                        .ok_or_else(|| EverEvoError::Internal(format!("Plugin '{name}' not found")))?;
                    let content = std::fs::read_to_string(&path)
                        .map_err(|e| EverEvoError::Internal(format!("Read: {e}")))?;
                    Ok(ToolOutput::text(format!(
                        "## {}/Cargo.toml\n\n{content}",
                        name
                    )))
                } else {
                    let (main_rs, cargo_toml) = self.read_source(category, name)
                        .map_err(EverEvoError::Internal)?;
                    let lines_main = main_rs.lines().count();
                    Ok(ToolOutput::text(format!(
                        "## plugins/{category}/{name}/src/main.rs ({lines_main} lines)\n\n{main_rs}\n\n## Cargo.toml\n\n{cargo_toml}",
                    )))
                }
            }

            "edit" => {
                let category = params["category"].as_str().unwrap_or("tools");
                let name = params["name"].as_str().unwrap_or("");
                let file = params["file"].as_str().unwrap_or("main.rs");
                let content = params["content"].as_str().unwrap_or("");
                if name.is_empty() || content.is_empty() {
                    return Ok(ToolOutput { content: "Provide 'name', 'category', and 'content' for action='edit'.".into(), is_error: true, ..Default::default() });
                }
                self.write_source(category, name, file, content)
                    .map_err(EverEvoError::Internal)?;
                let size = content.len();
                Ok(ToolOutput::text(format!(
                    "Plugin '{name}' {file} updated ({size} bytes).\n\
                     Next: plugin_dev(action='build', category='{category}', name='{name}') to compile.",
                )))
            }

            "build" => {
                let category = params["category"].as_str().unwrap_or("tools");
                let name = params["name"].as_str().unwrap_or("");
                if name.is_empty() {
                    return Ok(ToolOutput { content: "Provide 'name' and 'category' for action='build'.".into(), is_error: true, ..Default::default() });
                }
                match self.build_plugin(category, name) {
                    Ok(log) => Ok(ToolOutput::text(log)),
                    Err(e) => Ok(ToolOutput { content: e, is_error: true, ..Default::default() }),
                }
            }

            _ => Ok(ToolOutput {
                content: "Unknown action. Use 'list', 'source', 'edit', or 'build'.".into(),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
