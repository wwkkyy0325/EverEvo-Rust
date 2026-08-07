//! Shared helpers — title truncation, DB message conversion, permission,
//! git detection, and workspace context discovery.

use everevo_core::llm::{LlmMessage, LlmRole};
use everevo_db::models::MessageRow;

pub(crate) fn truncate_for_title(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    if first_line.chars().count() > 60 {
        first_line.chars().take(57).chain("...".chars()).collect()
    } else {
        first_line.to_string()
    }
}

pub(crate) fn db_message_to_llm(m: &MessageRow) -> LlmMessage {
    let role = match m.role.as_str() {
        "user" => LlmRole::User,
        "assistant" => LlmRole::Assistant,
        "system" => LlmRole::System,
        "tool" => LlmRole::Tool,
        _ => LlmRole::User,
    };
    // Only restore thinking for tool-call turns (DeepSeek Rule B).
    // Final answers without tool calls must drop thinking (Rule A).
    let has_tools = m
        .tool_calls
        .as_ref()
        .and_then(|tc| serde_json::from_str::<Vec<serde_json::Value>>(tc).ok())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);
    let thinking = if has_tools && !m.thinking.is_empty() {
        Some(m.thinking.clone())
    } else {
        None
    };
    LlmMessage {
        role,
        content: m.content.clone(),
        thinking,
        tool_calls: m
            .tool_calls
            .as_ref()
            .and_then(|tc| serde_json::from_str(tc).ok()),
        tool_call_id: m.tool_call_id.clone(),
        // Images are not persisted to DB — only carried in-memory for the
        // current turn. Reconstructed history is text-only by design.
        images: Vec::new(),
    }
}

pub(crate) fn resolve_permission(level: &str) -> everevo_sandbox::PermissionLevel {
    match level {
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        _ => everevo_sandbox::PermissionLevel::SemiAuto,
    }
}

// ── Git Detection ──────────────────────────────────────────────────────────

/// Detect git repository info for the workspace (Claude Code alignment).
/// Uses std::process to run git CLI — this runs at context-build time
/// (NOT inside the sandbox tool), so sandbox restrictions don't apply.
#[allow(clippy::disallowed_methods)]
pub(crate) fn detect_git(workspace: &std::path::Path) -> (Option<String>, Option<String>) {
    let git_dir = workspace.join(".git");
    if !git_dir.exists() {
        return (None, None);
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let modified = s
                .lines()
                .filter(|l| {
                    let trimmed = l.trim();
                    !trimmed.is_empty() && !trimmed.starts_with("??")
                })
                .count();
            let untracked = s.lines().filter(|l| l.trim().starts_with("??")).count();
            let mut parts = Vec::new();
            if modified > 0 {
                parts.push(format!("{modified} modified"));
            }
            if untracked > 0 {
                parts.push(format!("{untracked} untracked"));
            }
            if parts.is_empty() {
                "clean".to_string()
            } else {
                parts.join(", ")
            }
        });
    (branch, status)
}

// ── Workspace Context Discovery ─────────────────────────────────────────────

/// Walk up from workspace root discovering CLAUDE.md / AGENTS.md files
/// (Claude Code alignment — hierarchical context chain).
pub(crate) fn discover_workspace_context(workspace: &std::path::Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut current = Some(workspace.to_path_buf());
    while let Some(dir) = current {
        for name in &["CLAUDE.md", "AGENTS.md", ".everevo.md"] {
            let path = dir.join(name);
            if path.exists() && path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        files.push((path.display().to_string(), trimmed));
                    }
                }
            }
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    // Reverse so root-level files come first, workspace-level last (root-to-leaf)
    files.reverse();
    files
}
