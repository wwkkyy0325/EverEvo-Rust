//! ListDir built-in tool — structured directory listing for the workspace.
//!
//! Claude Code doesn't have a dedicated directory listing tool (it uses Bash `ls`),
//! but EverEvo runs on Windows where `ls`/`dir`/`Get-ChildItem` syntax varies.
//! A dedicated tool gives the LLM consistent structured output regardless of shell.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Resolve a user-supplied relative path against the workspace root and verify
/// it stays within bounds (no `../` escape). Returns the resolved path or an
/// error describing the escape attempt.
fn resolve_workspace_path(workspace_root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let joined = workspace_root.join(rel_path.trim_start_matches('/').trim_start_matches('\\'));

    let normalized = normalize_path(&joined);
    let normalized_root = normalize_path(workspace_root);

    if !normalized.starts_with(&normalized_root) {
        return Err(format!(
            "Path traversal blocked: '{}' is outside the workspace",
            rel_path
        ));
    }
    Ok(joined)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut components: Vec<std::path::Component<'_>> = Vec::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(c);
                }
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Maximum entries returned — prevents context blowout from large directories.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;
const MAX_DEPTH: usize = 3;

/// Directory entry metadata.
struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: String,
}

/// Lists files and directories in the workspace with structured output.
pub struct ListDirTool {
    workspace_root: PathBuf,
}

impl ListDirTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List files and directories in the workspace. \
         Returns structured output with names, types (📁/📄), sizes, \
         and modification times. Use to explore project structure. \
         Parameters: path (default: '.' — relative to workspace), \
         depth (1 = flat, 2-3 = recursive, default: 1), \
         limit (default: 50, max: 200)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to workspace root (default: '.')"
                },
                "depth": {
                    "type": "integer",
                    "description": "Recursion depth: 1 = flat, 2-3 = recursive (default: 1, max: 3)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max entries to return (default: 50, max: 200)"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low // read-only, only within workspace
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let subpath = params["path"].as_str().unwrap_or("");
        let target = if subpath.is_empty() || subpath == "." {
            self.workspace_root.clone()
        } else if std::path::Path::new(subpath).is_absolute() {
            return Ok(ToolOutput {
                content: format!(
                    "Absolute paths are not allowed. Use a path relative to workspace: {}",
                    self.workspace_root.display()
                ),
                is_error: true,
                ..Default::default()
            });
        } else {
            match resolve_workspace_path(&self.workspace_root, subpath) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ToolOutput {
                        content: e,
                        is_error: true,
                        ..Default::default()
                    });
                }
            }
        };

        if !target.exists() {
            return Ok(ToolOutput {
                content: format!("Path not found: {}", target.display()),
                is_error: true,
                ..Default::default()
            });
        }
        if !target.is_dir() {
            return Ok(ToolOutput {
                content: format!("Not a directory: {}", target.display()),
                is_error: true,
                ..Default::default()
            });
        }

        let depth = params["depth"].as_u64().unwrap_or(1).min(MAX_DEPTH as u64) as usize;
        let limit = params["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_LIMIT as u64)
            .min(MAX_LIMIT as u64) as usize;

        let entries = collect_entries(&target, depth, limit);
        if entries.is_empty() {
            return Ok(ToolOutput {
                content: format!("(empty directory)\n{}", target.display()),
                is_error: false,
                ..Default::default()
            });
        }

        let header = format!("{} ({} entries):\n", target.display(), entries.len());
        let lines: Vec<String> = entries
            .iter()
            .map(|e| {
                let icon = if e.is_dir { "📁" } else { "📄" };
                let size_str = if e.is_dir {
                    String::new()
                } else {
                    format!(" ({})", human_size(e.size))
                };
                format!("- {} `{}`{size_str} {}", icon, e.name, e.modified)
            })
            .collect();

        Ok(ToolOutput {
            content: header + &lines.join("\n"),
            is_error: false,
            ..Default::default()
        })
    }
}

/// Walk a directory and collect entries up to depth and limit.
fn collect_entries(root: &std::path::Path, max_depth: usize, limit: usize) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut dirs_to_visit: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, current_depth)) = dirs_to_visit.pop() {
        if current_depth >= max_depth || entries.len() >= limit {
            continue;
        }

        let dir_entries = match std::fs::read_dir(&dir) {
            Ok(iter) => iter,
            Err(_) => continue,
        };

        let mut batch: Vec<Entry> = Vec::new();
        for entry in dir_entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden files/dirs (but keep .git, .github for project context)
            if name.starts_with('.') && name != ".git" && name != ".github" && name != ".everevo" {
                continue;
            }
            // Skip common noise
            if name == "target" || name == "node_modules" || name == "__pycache__" {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = metadata.is_dir();
            let size = metadata.len();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| {
                    SystemTime::now()
                        .duration_since(t)
                        .ok()
                })
                .map(|age| {
                    let secs = age.as_secs();
                    if secs < 60 {
                        format!("{}s ago", secs)
                    } else if secs < 3600 {
                        format!("{}m ago", secs / 60)
                    } else if secs < 86400 {
                        format!("{}h ago", secs / 3600)
                    } else {
                        format!("{}d ago", secs / 86400)
                    }
                })
                .unwrap_or_else(|| "?".to_string());

            batch.push(Entry {
                name,
                is_dir,
                size,
                modified,
            });
        }

        // Sort: dirs first, then alphabetical
        batch.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        for entry in batch {
            if entries.len() >= limit {
                break;
            }
            entries.push(entry);
        }

        // Collect subdirs for recursive walk (add to front for depth-first)
        if current_depth + 1 < max_depth {
            if let Ok(iter) = std::fs::read_dir(&dir) {
                let mut subdirs: Vec<PathBuf> = iter
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        !name.starts_with('.')
                            || name == ".git"
                            || name == ".github"
                            || name == ".everevo"
                    })
                    .filter_map(|e| e.metadata().ok().and_then(|m| m.is_dir().then(|| e.path())))
                    .collect();
                // Reverse to maintain original order after stack pop
                subdirs.reverse();
                for sub in subdirs {
                    dirs_to_visit.push((sub, current_depth + 1));
                }
            }
        }
    }

    entries.truncate(limit);
    entries
}

/// Format a byte size as human-readable.
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1048576), "1.0 MB");
        assert_eq!(human_size(1073741824), "1.0 GB");
    }

    #[test]
    fn test_name_and_schema() {
        let tool = ListDirTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "list_dir");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
    }
}
