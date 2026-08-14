//! Token budget allocation for context assembly — adapts per model
//! `context_window` so prompt assembly tracks the model actually in use.
//!
//! **128k is a floor, not a clamp**: `resolve(None)` uses `DEFAULT_CONTEXT_WINDOW`,
//! but `resolve(Some(32_768))` (e.g. a small vision model) keeps 32k. Small
//! windows are never inflated up to the floor.
//!
//! Budget layout (window = effective window after floor):
//!
//! ```text
//! window = safety_margin(10%) + output_reserve((window/50).clamp(2k, 8k)) + available
//! available = fixed(14.5%) + memory+domain(15%) + rolling_summary(4%) + history(余量)
//! ```
//!
//! Invariant: `fixed + memory + summary + history == available`.

use crate::llm::LlmMessage;

/// Floor applied only when no `context_window` is configured.
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Per-stage token cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageBudget {
    pub name: &'static str,
    pub budget_tokens: usize,
}

/// Token allocation for one context assembly.
///
/// `window == 0` is the "legacy" sentinel (Default): stages that consult the
/// budget fall back to their historical behavior (message-count window, 40%
/// oversized heuristic).
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Effective window post-floor (only `None` is floored; small `Some(w)` kept).
    pub window: usize,
    /// Reserved headroom so the model never sees a full window.
    pub safety_margin: usize,
    /// Reserved for the model's completion tokens.
    pub output_reserve: usize,
    /// `window - safety_margin - output_reserve`.
    pub available: usize,
    /// Per-stage caps for the fixed-cost stages.
    pub fixed: [StageBudget; 10],
    /// Shared cap for memory + domain/RAG stages.
    pub memory_budget: usize,
    /// Cap for the rolling summary stage.
    pub summary_budget: usize,
    /// Everything left — the conversation history window.
    pub history_budget: usize,
}

/// Relative weights for the 10 fixed stages (sums to 100).
const FIXED_WEIGHTS: [(&str, usize); 10] = [
    ("system_prompt", 25),
    ("session_metadata", 15),
    ("task_state", 10),
    ("persona", 12),
    ("best_practices", 12),
    ("skill", 8),
    ("workspace_context", 10),
    ("todo_summary", 4),
    ("hook_feedback", 2),
    ("latest_message", 2),
];

impl ContextBudget {
    /// Build a budget for a provider's configured context window.
    pub fn resolve(window: Option<u32>) -> Self {
        let raw = window
            .map(|w| w as usize)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW)
            .max(1_000); // guard against 0/1 windows, but don't inflate small ones
        let safety_margin = raw / 10;
        let output_reserve = (raw / 50).clamp(2_048, 8_192);
        // Saturating: a tiny window (≤ ~2.5k) cannot fit the clamped output
        // reserve, so `available` degrades toward 0 rather than underflowing.
        let available = raw
            .saturating_sub(safety_margin)
            .saturating_sub(output_reserve);
        let fixed_total = available * 145 / 1000; // 14.5%
        let memory_budget = available * 15 / 100; // 15%
        let summary_budget = available * 4 / 100; // 4%
        let history_budget = available - fixed_total - memory_budget - summary_budget;

        let total_weight: usize = FIXED_WEIGHTS.iter().map(|(_, w)| w).sum();
        let mut fixed = [StageBudget {
            name: "",
            budget_tokens: 0,
        }; 10];
        let mut assigned = 0usize;
        for (i, (name, weight)) in FIXED_WEIGHTS.iter().enumerate() {
            let tokens = if i == FIXED_WEIGHTS.len() - 1 {
                fixed_total - assigned // last takes the remainder so the sum is exact
            } else {
                fixed_total * weight / total_weight
            };
            assigned += tokens;
            fixed[i] = StageBudget {
                name,
                budget_tokens: tokens,
            };
        }

        Self {
            window: raw,
            safety_margin,
            output_reserve,
            available,
            fixed,
            memory_budget,
            summary_budget,
            history_budget,
        }
    }

    /// Return the token cap for a named stage. Returns 0 for unknown stages —
    /// callers treat 0 as "no explicit cap".
    pub fn stage(&self, name: &str) -> usize {
        if let Some(sb) = self.fixed.iter().find(|s| s.name == name) {
            return sb.budget_tokens;
        }
        match name {
            "memory" | "domain_knowledge" => self.memory_budget,
            "rolling_summary" => self.summary_budget,
            "conversation_history" => self.history_budget,
            _ => 0,
        }
    }

    /// Newest-first token-budget sliding window: accumulate history from the
    /// newest message backwards while within `history_budget`, return the
    /// contiguous newest slice (oldest-first ordering preserved for the prompt).
    pub fn history_window<'a>(&self, history: &'a [LlmMessage]) -> &'a [LlmMessage] {
        let mut tokens = 0usize;
        let mut start = history.len();
        for (i, msg) in history.iter().enumerate().rev() {
            tokens += estimate_tokens(&msg.content);
            if tokens > self.history_budget {
                break;
            }
            start = i;
        }
        &history[start..]
    }
}

impl Default for ContextBudget {
    /// Legacy sentinel — `window == 0`; callers fall back to old behavior.
    fn default() -> Self {
        Self {
            window: 0,
            safety_margin: 0,
            output_reserve: 0,
            available: 0,
            fixed: [StageBudget {
                name: "",
                budget_tokens: 0,
            }; 10],
            memory_budget: 0,
            summary_budget: 0,
            history_budget: 0,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_none_uses_128k_floor() {
        let b = ContextBudget::resolve(None);
        assert_eq!(b.window, DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn resolve_small_window_not_inflated() {
        // Floor is NOT a clamp: a 32k vision model stays at 32k.
        let b = ContextBudget::resolve(Some(32_768));
        assert_eq!(b.window, 32_768);
        assert!(b.window < DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn resolve_tiny_window_guarded() {
        let b = ContextBudget::resolve(Some(0));
        assert_eq!(b.window, 1_000);
    }

    #[test]
    fn invariant_fixed_plus_memory_plus_summary_plus_history_equals_available() {
        for window in [
            Some(32_768u32),
            Some(131_072),
            Some(200_000),
            Some(1_000_000),
            None,
        ] {
            let b = ContextBudget::resolve(window);
            let fixed_total: usize = b.fixed.iter().map(|s| s.budget_tokens).sum();
            assert_eq!(
                b.available,
                fixed_total + b.memory_budget + b.summary_budget + b.history_budget,
                "invariant broken for window {window:?}"
            );
        }
    }

    #[test]
    fn output_reserve_is_clamped() {
        let small = ContextBudget::resolve(Some(131_072));
        let huge = ContextBudget::resolve(Some(1_000_000));
        assert!(small.output_reserve >= 2_048);
        assert_eq!(huge.output_reserve, 8_192); // clamped, not 20k
    }

    #[test]
    fn stage_returns_fixed_cap() {
        let b = ContextBudget::resolve(None);
        assert!(b.stage("system_prompt") > 0);
        assert!(b.stage("latest_message") > 0);
    }

    #[test]
    fn stage_returns_group_caps() {
        let b = ContextBudget::resolve(None);
        assert_eq!(b.stage("memory"), b.memory_budget);
        assert_eq!(b.stage("domain_knowledge"), b.memory_budget);
        assert_eq!(b.stage("rolling_summary"), b.summary_budget);
        assert_eq!(b.stage("conversation_history"), b.history_budget);
    }

    #[test]
    fn stage_unknown_returns_zero() {
        let b = ContextBudget::resolve(None);
        assert_eq!(b.stage("does_not_exist"), 0);
    }

    #[test]
    fn history_window_keeps_newest_within_budget() {
        let b = ContextBudget::resolve(Some(131_072));
        let mut history = Vec::new();
        for i in 0..200 {
            history.push(LlmMessage::user(format!("message number {i}")));
        }
        let win = b.history_window(&history);
        assert!(!win.is_empty());
        assert_eq!(win.last().unwrap().content.as_str(), "message number 199"); // newest kept
    }

    #[test]
    fn history_window_budget_respected() {
        let b = ContextBudget::resolve(Some(131_072));
        // Each message ~ (18 chars)/4 ≈ 4 tokens. 200 msgs ≈ 800 tokens — all fit.
        let mut history = Vec::new();
        for i in 0..200 {
            history.push(LlmMessage::user(format!("m{i}")));
        }
        let win = b.history_window(&history);
        assert_eq!(win.len(), 200);
        let _ = b; // silence unused in this branch
    }

    #[test]
    fn default_is_legacy_sentinel() {
        let b = ContextBudget::default();
        assert_eq!(b.window, 0);
        assert_eq!(b.stage("memory"), 0);
    }
}

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
    /// Tokens usable for the prompt (window − safety − output reserve).
    pub available_tokens: usize,
    /// Headroom reserved so the model never sees a full window.
    pub safety_reserved: usize,
    /// Tokens reserved for the model's completion.
    pub output_reserved: usize,
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
    /// Per-stage token budget resolved from the model's `context_window`.
    /// `window == 0` (Default) means unset — stages fall back to legacy behavior.
    pub budget: ContextBudget,

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
    /// Set after each tool execution; read by AgentRun for next-turn injection.
    pub hook_feedback: Option<String>,
    /// Durable rolling conversation summary (spec D3). Maintained incrementally
    /// in the background (Layer-1) and persisted to the sessions table; injected
    /// before the sliding-window history so the model sees what happened before
    /// the window. Never re-summarized (rule 1).
    pub summary: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod data_tests {
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
