use crate::llm::LlmMessage;

// ── Context Fragment ────────────────────────────────────────────────────

/// A piece of context produced by a single stage.
#[derive(Debug, Clone)]
pub struct ContextFragment {
    /// Human-readable label for debugging / log output.
    pub label: String,
    /// One or more messages this stage contributes to the prompt.
    pub messages: Vec<LlmMessage>,
}

// ── Context Observability ────────────────────────────────────────────────

/// Snapshot of a single stage's contribution during context assembly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StageSnapshot {
    /// Stage name from ContextStage::name().
    pub stage_name: String,
    /// Priority from ContextStage::priority().
    pub priority: i32,
    /// Whether the stage contributed (true) or returned None (false).
    pub contributed: bool,
    /// Human-readable label from the fragment, if contributed.
    pub label: Option<String>,
    /// Number of LlmMessages the stage contributed.
    pub message_count: usize,
    /// Combined content of all messages from this stage, truncated for API.
    pub content_preview: Option<String>,
    /// Estimated token count using the char-based heuristic.
    pub estimated_tokens: usize,
    /// Auto-detected status: "ok", "warn", "missing", "oversized".
    pub status: String,
}

/// Complete snapshot for one turn of context assembly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextSnapshot {
    /// Session UUID this snapshot belongs to.
    pub session_id: uuid::Uuid,
    /// Monotonic turn number within the session.
    pub turn_number: usize,
    /// ISO-8601 timestamp when the snapshot was captured.
    pub captured_at: String,
    /// Per-stage breakdown, in priority order.
    pub stages: Vec<StageSnapshot>,
    /// Total estimated tokens across all stages.
    pub total_estimated_tokens: usize,
    /// Configured context window budget.
    pub max_context_tokens: usize,
    /// Percentage of budget used (0.0–100.0+).
    pub budget_used_pct: f64,
    /// Auto-detected flags (empty vec if all clear).
    pub flags: Vec<String>,
}

/// Max chars per stage in API response content preview.
pub const CONTEXT_PREVIEW_MAX_CHARS: usize = 2000;

/// Estimate token count from a string using the char-based heuristic.
/// CJK characters (Chinese, Japanese, Korean) estimate at 2 chars/token.
/// All other characters estimate at 4 chars/token.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        let code = ch as u32;
        if (0x4E00..=0x9FFF).contains(&code)
            || (0x3400..=0x4DBF).contains(&code)
            || (0xF900..=0xFAFF).contains(&code)
            || (0x3000..=0x303F).contains(&code)
            || (0xFF00..=0xFFEF).contains(&code)
            || (0xAC00..=0xD7AF).contains(&code)
        {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    (cjk / 2) + (other / 4)
}

/// Truncate content to max_chars (at char boundary), adding "…" if truncated.
pub(crate) fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

// ── Build Context ───────────────────────────────────────────────────────

/// Information available to each stage during context assembly.
///
/// Fields are added as the system grows; stages ignore what they don't need.
#[derive(Debug, Clone, Default)]
pub struct ContextBuildContext {
    /// The latest user message (raw text).
    pub user_message: String,
    /// Session identifier, if one exists.
    pub session_id: Option<uuid::Uuid>,
    /// Session title.
    pub session_title: Option<String>,
    /// Previous messages in this session (oldest first), already loaded from DB.
    pub history: Vec<LlmMessage>,
    /// Total tokens in `history` (approximate), for sliding-window decisions.
    pub history_tokens: usize,
    /// Maximum tokens the model can accept (context window limit).
    pub max_context_tokens: usize,

    // ── Runtime Environment (dynamic, injected per-request) ─────────
    /// Active shell (e.g., "Git Bash", "PowerShell", "WSL")
    pub shell_name: Option<String>,
    /// Current permission level label (e.g., "半自动")
    pub permission_level: Option<String>,
    /// User-trusted paths
    pub trusted_paths: Vec<String>,
    /// Number of registered tools
    pub tool_count: usize,
    /// Primary working directory path (workspace or sandbox).
    /// Shown in the system prompt so the LLM knows where it's working.
    pub workspace_path: Option<String>,
    /// OS platform identifier (e.g., "win32", "linux", "darwin").
    pub platform: Option<String>,
    /// Git branch name (e.g., "main") if workspace is a git repo.
    pub git_branch: Option<String>,
    /// Summarized git status (e.g., "2 modified, 1 untracked") if available.
    pub git_status: Option<String>,
    /// Workspace context files discovered by walk-up (CLAUDE.md, AGENTS.md, etc.).
    /// Each entry: (absolute_path, file_content).
    pub workspace_context_files: Vec<(String, String)>,
    /// Current date in YYYY-MM-DD format (Claude Code alignment).
    pub current_date: Option<String>,
    /// Summary of current TodoWrite task list (pending/in_progress/completed).
    /// Injected so the agent can distinguish done from pending work
    /// and correctly interpret "继续" (continue) as "resume pending".
    pub todo_summary: Option<String>,
    /// Whether the current session is in plan mode (read-only exploration).
    /// When true, BestPracticesStage injects the 5-phase workflow.
    pub plan_mode: bool,
    /// Credential config removed — sandbox now inherits host git config directly.
    /// Global ~/.gitconfig and ~/.ssh are used as-is.
    /// Summary of available portable runtimes (Python, Node, Git, ONNX).
    /// e.g. "Python 3.12.8, Node v22.12.0, Git 2.47.1, ONNX Runtime ✅"
    /// Built from startup_check results; displayed in SessionMetadataStage.
    pub runtime_summary: Option<String>,
    /// The sandbox root directory path. Helps the LLM understand what's
    /// inside vs outside the isolation boundary.
    pub sandbox_root: Option<String>,
    /// Whether the startup self-check passed all critical tests.
    /// When true, the LLM should trust that ONNX, SQLite, and runtimes work.
    pub startup_verified: bool,
    /// Feedback from ReflectGate sync quick-check (hook_feedback).
    /// Set after each tool execution; read by AgentLoop for next-turn injection.
    pub hook_feedback: Option<String>,
    /// Durable rolling conversation summary (spec D3). Maintained incrementally
    /// in the background (Layer-1) and persisted to the sessions table; injected
    /// before the sliding-window history so the model sees what happened before
    /// the window. Never re-summarized (rule 1).
    pub summary: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── estimate_tokens ───────────────────────────────────────────────

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_ascii() {
        assert_eq!(estimate_tokens("hello world"), 11 / 4);
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        assert_eq!(estimate_tokens("你好世界"), 4 / 2);
    }

    #[test]
    fn test_estimate_tokens_mixed() {
        let tokens = estimate_tokens("Hello你好");
        assert_eq!(tokens, 2);
    }

    #[test]
    fn test_estimate_tokens_korean() {
        assert!(estimate_tokens("한글") > 0);
    }

    // ── truncate_content ──────────────────────────────────────────────

    #[test]
    fn test_truncate_content_under_limit() {
        assert_eq!(truncate_content("short", 100), "short");
    }

    #[test]
    fn test_truncate_content_at_exact_limit() {
        assert_eq!(truncate_content("exact", 5), "exact");
    }

    #[test]
    fn test_truncate_content_over_limit() {
        assert_eq!(truncate_content("hello world", 5), "hello…");
    }

    #[test]
    fn test_truncate_content_cjk() {
        assert_eq!(truncate_content("你好世界测试", 3), "你好世…");
    }

    // ── ContextBuildContext ────────────────────────────────────────────

    #[test]
    fn test_context_build_context_default() {
        let ctx = ContextBuildContext::default();
        assert!(ctx.user_message.is_empty());
        assert!(ctx.session_id.is_none());
        assert!(ctx.history.is_empty());
        assert_eq!(ctx.history_tokens, 0);
        assert_eq!(ctx.max_context_tokens, 0);
        assert!(!ctx.plan_mode);
    }
}
