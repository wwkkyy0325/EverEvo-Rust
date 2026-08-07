//! Code search tools — keyword search + directory map for the LLM.
//!
//! ## Architecture (research-backed, Phases 0-2)
//!
//! - `code_search` — FTS5 trigram index (primary) + ripgrep fallback (secondary)
//! - `code_map` — lightweight Markdown directory overview (CLAUDE.md pattern)
//!
//! ### Search Pipeline
//! ```text
//! query → [<3 chars? → rg] → [index ready? → FTS5 trigram] → [0 results? → rg]
//!        → [dedup + identifier-weighted rank] → [hard caps 12×250] → [compact format]
//! ```
//!
//! Design sources:
//! - Trigram FTS5: Sourcegraph Zoekt architecture (P99 50ms)
//! - Grep fallback: Claude Code grep-first agentic search
//! - Dedup + ranking: GrepRAG (ISSTA 2026, Zhejiang Univ)
//! - Hard caps: Cody production (12 results × 250 tokens)
//! - Compact format: Progressive disclosure pattern

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::code_search::{format_search_results, CodeIndex, SearchConfig};

// ── CodeSearch tool ──────────────────────────────────────────────────────

pub struct CodeSearchTool {
    index: Arc<tokio::sync::Mutex<Option<CodeIndex>>>,
    /// Signaled when background index build completes (success or failure).
    index_ready: Arc<Notify>,
    workspace_root: PathBuf,
}

impl CodeSearchTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            index: Arc::new(tokio::sync::Mutex::new(None)),
            index_ready: Arc::new(Notify::new()),
            workspace_root,
        }
    }

    /// Trigger background indexing without blocking.
    /// Uses smart_reindex: full rebuild if index is empty, incremental otherwise.
    /// Marks `last_indexed` on completion so the polling check in `execute()`
    /// can detect staleness and auto-reindex.
    pub fn start_background_index(&self) {
        let index = Arc::clone(&self.index);
        let ready = Arc::clone(&self.index_ready);
        let root = self.workspace_root.clone();
        tokio::spawn(async move {
            let db_path = root.join(".everevo").join("code_index.db");
            match CodeIndex::open(&db_path, &root).await {
                Ok(mut idx) => {
                    tracing::info!(path = %root.display(), "Background code indexing started");
                    match idx.smart_reindex().await {
                        Ok(stats) => {
                            tracing::info!(
                                symbols = stats.symbols,
                                files = stats.files,
                                elapsed_ms = stats.elapsed_ms,
                                "Code indexing complete"
                            );
                            idx.wal_checkpoint().await;
                        }
                        Err(e) => tracing::warn!(error = %e, "Code indexing failed"),
                    }
                    idx.last_indexed = Some(std::time::Instant::now());
                    *index.lock().await = Some(idx);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to open code index for background build")
                }
            }
            ready.notify_waiters();
        });
    }

    /// Check whether files have changed since the last index and, if so,
    /// run an incremental reindex. Called on every `code_search` invocation
    /// so the index stays fresh without manual `reindex: true`.
    async fn auto_reindex_if_stale(&self) {
        let mut guard = self.index.lock().await;
        if let Some(ref mut idx) = *guard {
            // Only check once every 10 seconds to avoid thrashing.
            if let Some(last) = idx.last_indexed {
                if last.elapsed().as_secs() < 10 {
                    return;
                }
            }
            // Drop the guard briefly so reindex_changed can acquire the pool
        }
        drop(guard);

        // Check for changes in a blocking task (walkdir is sync I/O)
        let root = self.workspace_root.clone();
        let needs_reindex = tokio::task::spawn_blocking(move || {
            // Compare against 20 s ago as a safety margin
            let since = SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(20))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            crate::code_search::has_changes_since(&root, since)
        })
        .await
        .unwrap_or(false);

        if !needs_reindex {
            // Update the timestamp so we don't re-check for 10 s
            let mut guard = self.index.lock().await;
            if let Some(ref mut idx) = *guard {
                idx.last_indexed = Some(std::time::Instant::now());
            }
            return;
        }

        let mut guard = self.index.lock().await;
        if let Some(ref mut idx) = *guard {
            tracing::debug!("Polling detected file changes — auto reindexing");
            match idx.reindex_changed().await {
                Ok(stats) => {
                    if stats.files > 0 {
                        tracing::info!(
                            symbols = stats.symbols,
                            files = stats.files,
                            "Auto-reindex: code index updated"
                        );
                    }
                    idx.wal_checkpoint().await;
                }
                Err(e) => tracing::warn!(error = %e, "Auto-reindex failed"),
            }
            idx.last_indexed = Some(std::time::Instant::now());
        }
    }

    /// Wait for the background index build (up to 120s).
    pub async fn ensure_index(&self) -> Result<(), String> {
        {
            let guard = self.index.lock().await;
            if guard.is_some() {
                return Ok(());
            }
        }
        let timeout = std::time::Duration::from_secs(120);
        tokio::select! {
            _ = self.index_ready.notified() => {
                let guard = self.index.lock().await;
                if guard.is_some() { Ok(()) } else {
                    Err("Code index build failed in background. Delete .everevo/code_index.db and retry.".into())
                }
            }
            _ = tokio::time::sleep(timeout) => {
                Err("Code index is still being built (120s timeout). Use shell+grep for ad-hoc search instead.".into())
            }
        }
    }

    /// Run ripgrep as fallback when the index is unavailable or query is too short.
    /// Returns results in the same compact format as indexed search.
    ///
    /// Tries `rg` first, then `grep -rn` (Git Bash / MSYS2 / WSL), then PowerShell
    /// `Select-String` on Windows. This avoids the silent-failure path where rg
    /// doesn't exist on PATH and the tool returns nothing useful.
    #[allow(clippy::disallowed_methods)] // rg/grep are read-only, same as read_file/list_dir
    async fn run_grep_fallback(&self, query: &str, kind: Option<&str>, limit: usize) -> String {
        let kind_pattern = kind.map(|k| match k {
            "fn" => format!("fn\\s+{query}"),
            "struct" => format!("struct\\s+{query}"),
            "trait" => format!("trait\\s+{query}"),
            "enum" => format!("enum\\s+{query}"),
            "mod" => format!("mod\\s+{query}"),
            "type" => format!("type\\s+{query}"),
            "const" => format!("const\\s+{query}"),
            "impl" => format!("impl\\s+.*{query}"),
            _ => query.to_string(),
        });
        let pattern = kind_pattern.as_deref().unwrap_or(query);

        let max_count = (limit * 3).min(50);

        // ── Tier 1: ripgrep (fastest, best output) ──
        let rg_result = tokio::process::Command::new("rg")
            .args(["--no-heading", "-n", "-i", pattern, "--type-add"])
            .arg("code:*.{rs,ts,tsx,js,jsx,py,go,java,kt,rb,swift,c,cpp,h,hpp}")
            .args(["-t", "code"])
            .args(["-l"])
            .current_dir(&self.workspace_root)
            .output()
            .await;

        match rg_result {
            Ok(output) if output.status.success() => {
                return format_grep_output(&output.stdout, query, max_count, "rg");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(stderr = %stderr, "rg exited non-zero, trying grep fallback");
            }
            Err(_) => {
                tracing::debug!("rg not on PATH, trying grep fallback");
            }
        }

        // ── Tier 2: grep -rn (Git Bash / MSYS2 / WSL / macOS / Linux) ──
        let grep_result = tokio::process::Command::new("grep")
            .args(["-rn", "-i", pattern])
            .arg("--include=*.rs")
            .arg("--include=*.ts")
            .arg("--include=*.tsx")
            .arg("--include=*.js")
            .arg("--include=*.jsx")
            .arg("--include=*.py")
            .arg("--include=*.go")
            .arg("--include=*.java")
            .arg("--include=*.kt")
            .arg("--include=*.c")
            .arg("--include=*.cpp")
            .arg("--include=*.h")
            .arg("--include=*.hpp")
            .args(["-l"])
            .current_dir(&self.workspace_root)
            .output()
            .await;

        match grep_result {
            Ok(output) if output.status.success() => {
                return format_grep_output(&output.stdout, query, max_count, "grep");
            }
            Ok(_) | Err(_) => {
                tracing::debug!("grep not on PATH or no matches, trying PowerShell fallback");
            }
        }

        // ── Tier 3: PowerShell Select-String (Windows built-in) ──
        #[cfg(windows)]
        {
            let ps_pattern = kind_pattern.as_deref().unwrap_or(query);
            let ps_script = format!(
                "Get-ChildItem -Recurse -File -Include *.rs,*.ts,*.tsx,*.js,*.jsx,*.py,*.go,*.java,*.kt,*.c,*.cpp,*.h,*.hpp | \
                 Select-String -Pattern '{ps_pattern}' -List | \
                 ForEach-Object {{ $_.Path }}",
            );
            let ps_result = tokio::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &ps_script])
                .current_dir(&self.workspace_root)
                .output()
                .await;

            match ps_result {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let files: Vec<String> = stdout
                        .lines()
                        .filter(|l| !l.is_empty())
                        .take(max_count)
                        .map(|s| {
                            // Make path relative to workspace root
                            let p = std::path::Path::new(s.trim());
                            p.strip_prefix(&self.workspace_root)
                                .unwrap_or(p)
                                .to_string_lossy()
                                .replace('\\', "/")
                        })
                        .collect();
                    if files.is_empty() {
                        return format!("No matches found for '{query}'.");
                    }
                    let count = files.len();
                    let mut out = format!("{count} files matching '{query}' (PowerShell Select-String):\n");
                    for f in &files {
                        out.push_str(&format!("- `{f}`\n"));
                    }
                    if count >= max_count {
                        out.push_str(&format!(
                            "\n[{count} total matches, showing top {max_count}.]\n"
                        ));
                    }
                    out.push_str("\nUse `read_file` to inspect individual files.\n");
                    return out;
                }
                _ => {}
            }
        }

        format!(
            "No search tool available. All of rg, grep, and PowerShell Select-String are unavailable.\n\
             Use the `shell` tool for ad-hoc search:\n\
             `grep -rn '{query}' .` or `Select-String -Path . -Pattern '{query}' -Recurse`"
        )
    }
}

/// Format the stdout output from `rg -l` or `grep -rnl` into the standard
/// compact result block.
fn format_grep_output(stdout: &[u8], query: &str, max_count: usize, tool_name: &str) -> String {
    let files: Vec<String> = String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .take(max_count)
        .map(|s| s.to_string())
        .collect();

    if files.is_empty() {
        return format!("No matches found for '{query}' with {tool_name}. Try a different keyword.");
    }

    let count = files.len();
    let mut out = format!("{count} files matching '{query}' ({tool_name} fallback):\n");
    for f in &files {
        out.push_str(&format!("- `{f}`\n"));
    }
    if count >= max_count {
        out.push_str(&format!(
            "\n[{count} total matches, showing top {max_count}. Refine your query for more precision.]\n"
        ));
    }
    out.push_str("\nUse `read_file` to inspect individual files.\n");
    out
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "Search the codebase for symbols and code patterns using a trigram FTS5 index. \
         Automatically falls back to ripgrep when the index is unavailable or a query is \
         too short (<3 chars). Returns compact file:line+sig results (12 max, 200 chars/sig) \
         to minimize context pollution. Use `read_file` to inspect matches in detail. \
         Parameters: query (required — ≥3 chars for index, uses rg otherwise), \
         kind (optional — filter by fn/struct/impl/trait/enum/mod/type/const), \
         limit (optional — default 10), \
         expand (optional — return full signatures instead of truncated), \
         reindex (optional — refresh index before searching)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name or keyword (≥3 chars for FTS5 trigram; <3 chars auto-uses rg)"
                },
                "kind": {
                    "type": "string",
                    "enum": ["fn", "struct", "impl", "trait", "enum", "mod", "type", "const"],
                    "description": "Filter by symbol type (optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default: 10, max: 12 due to context budget)"
                },
                "expand": {
                    "type": "boolean",
                    "description": "Return full signatures instead of truncated (default: false)"
                },
                "reindex": {
                    "type": "boolean",
                    "description": "Refresh the index before searching (use after file changes)"
                }
            },
            "required": ["query"]
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
        let query = params["query"].as_str().unwrap_or("");
        if query.is_empty() {
            return Ok(ToolOutput {
                content: "query is required".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let limit = params["limit"].as_u64().unwrap_or(10).min(12) as usize;
        let kind = params["kind"].as_str();
        let expand = params["expand"].as_bool().unwrap_or(false);
        let do_reindex = params["reindex"].as_bool().unwrap_or(false);

        // ── Auto-reindex on staleness (polling, no watcher dependency) ──
        // Runs before the search; skips if checked <10 s ago.
        self.auto_reindex_if_stale().await;

        let config = SearchConfig {
            max_results: limit,
            expand,
            ..Default::default()
        };

        // ── Short query → rg fallback (trigram needs ≥3 chars) ──
        if query.len() < config.min_query_len {
            tracing::info!(query = %query, len = query.len(), "Query too short for trigram index, using rg fallback");
            let output = self.run_grep_fallback(query, kind, limit).await;
            return Ok(ToolOutput {
                content: output,
                is_error: false,
                ..Default::default()
            });
        }

        // ── Reindex if requested ──
        if do_reindex {
            let guard = self.index.lock().await;
            if let Some(ref index) = *guard {
                if let Err(e) = index.reindex_changed().await {
                    return Ok(ToolOutput {
                        content: format!("Reindex failed: {e}"),
                        is_error: true,
                        ..Default::default()
                    });
                }
            }
        }

        // ── Try index; fall back to rg on failure ──
        match self.ensure_index().await {
            Ok(()) => {
                let guard = self.index.lock().await;
                let index = guard
                    .as_ref()
                    .ok_or_else(|| EverEvoError::Internal("index not initialized".into()))?;

                let results = if let Some(k) = kind {
                    index.search_by_kind(k, query, limit).await
                } else {
                    index.search(query, limit).await
                };

                match results {
                    Ok(ref r) if r.is_empty() => {
                        // Index returned 0 results — try rg as follow-up
                        tracing::info!(query = %query, "FTS5 returned 0 results, trying rg fallback");
                        let output = self.run_grep_fallback(query, kind, limit).await;
                        Ok(ToolOutput {
                            content: output,
                            is_error: false,
                            ..Default::default()
                        })
                    }
                    Ok(r) => {
                        let formatted = format_search_results(&r, query, &config);
                        Ok(ToolOutput {
                            content: formatted,
                            is_error: false,
                            ..Default::default()
                        })
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "FTS5 search error, trying rg fallback");
                        let output = self.run_grep_fallback(query, kind, limit).await;
                        Ok(ToolOutput {
                            content: output,
                            is_error: false,
                            ..Default::default()
                        })
                    }
                }
            }
            Err(e) => {
                // Index not available — automatic rg fallback
                tracing::info!(error = %e, "Index unavailable, using rg fallback");
                let output = self.run_grep_fallback(query, kind, limit).await;
                Ok(ToolOutput {
                    content: output,
                    is_error: false,
                    ..Default::default()
                })
            }
        }
    }
}

// ── CodeMap tool ─────────────────────────────────────────────────────────

pub struct CodeMapTool {
    workspace_root: PathBuf,
}

impl CodeMapTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for CodeMapTool {
    fn name(&self) -> &str {
        "code_map"
    }

    fn description(&self) -> &str {
        "Return a lightweight Markdown directory overview of the codebase. \
         Shows the top-level structure with one-line descriptions inferred \
         from directory names and key files. Use this to understand the \
         project layout before diving into specific directories. \
         Parameters: path (optional — subdirectory to map, defaults to root)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory to map (default: workspace root)"
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
        let subpath = params["path"].as_str().unwrap_or("");
        let target = if subpath.is_empty() {
            self.workspace_root.clone()
        } else {
            self.workspace_root.join(subpath.trim_start_matches('/'))
        };

        let mut map = String::from("# Codebase Map\n\n");
        map.push_str(&format!("## {}\n\n", target.display()));

        // Read entries, skip hidden/system dirs
        let mut entries: Vec<_> = match std::fs::read_dir(&target) {
            Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Cannot read directory: {e}"),
                    is_error: true,
                    ..Default::default()
                })
            }
        };
        entries.sort_by_key(|e| {
            (
                e.file_type().map(|t| !t.is_dir()).unwrap_or(true),
                e.file_name(),
            )
        });

        for entry in &entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }

            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let prefix = if is_dir { "📁" } else { "📄" };

            // Read first line of README/Cargo.toml/package.json for context
            let desc = if is_dir {
                let readme = entry.path().join("README.md");
                if readme.exists() {
                    first_line_desc(&readme)
                } else {
                    let cargo = entry.path().join("Cargo.toml");
                    if cargo.exists() {
                        first_line_desc(&cargo)
                    } else {
                        let pkg = entry.path().join("package.json");
                        if pkg.exists() {
                            first_line_desc(&pkg)
                        } else {
                            String::new()
                        }
                    }
                }
            } else {
                first_line_desc(&entry.path())
            };

            map.push_str("- ");
            map.push_str(prefix);
            map.push_str(" `");
            map.push_str(&name);
            map.push('`');
            if !desc.is_empty() {
                map.push_str(" — ");
                map.push_str(&desc);
            }
            map.push('\n');
        }

        Ok(ToolOutput {
            content: map,
            is_error: false,
            ..Default::default()
        })
    }
}

fn first_line_desc(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| {
            c.lines().next().map(|l| {
                l.trim()
                    .trim_start_matches("# ")
                    .trim_start_matches("// ")
                    .to_string()
            })
        })
        .unwrap_or_default()
        .chars()
        .take(100)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_search_name_and_schema() {
        let tool = CodeSearchTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "code_search");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "query");
    }

    #[test]
    fn test_code_map_name_and_schema() {
        let tool = CodeMapTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "code_map");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
    }
}
