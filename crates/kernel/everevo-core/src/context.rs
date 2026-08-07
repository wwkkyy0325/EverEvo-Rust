//! Context injection pipeline — composes the full LLM prompt context.
//!
//! ## Architecture
//!
//! Each call to the LLM assembles a context from multiple *stages* executed in
//! priority order. Stages are trait objects, making the pipeline extensible:
//! future work (RAG, knowledge graph, tool definitions) adds new stages
//! without touching the core chat logic.
//!
//! ## Design Reference
//!
//! ChatGPT's 7-layer context assembly (reverse-engineered by Manthan Gupta):
//! system instructions → user memory → session metadata → recent summaries →
//! current messages → latest input. We mirror this with pluggable stages.
//!
//! ```text
//! [0] System Prompt         ← static, loaded from config
//! [1] User Memory           ← persistent facts (future)
//! [2] Session Metadata      ← ephemeral per-session
//! [3] Recent Sessions       ← cross-session context (future)
//! [4] Knowledge Base        ← RAG results slot (future)
//! [5] Tool Definitions      ← available tools slot (future)
//! [6] Conversation History  ← current session messages, sliding window
//! [7] Latest User Message   ← the new input
//! ```

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
fn truncate_content(content: &str, max_chars: usize) -> String {
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
}

// ── Context Stage Trait ─────────────────────────────────────────────────

/// A single stage in the context injection pipeline.
///
/// Implementors return `None` when they have nothing to contribute for the
/// current turn (e.g., KnowledgeBase returns `None` when no relevant docs
/// are found, UserMemory when no facts are stored, etc.).
pub trait ContextStage: Send + Sync {
    /// Execution order — lower runs first (appears earlier in the prompt).
    fn priority(&self) -> i32;

    /// Short name for logging (`"system_prompt"`, `"history"`, …).
    fn name(&self) -> &str;

    /// Build the context fragment for this turn.
    ///
    /// Return `None` if the stage has nothing to contribute — it is simply
    /// skipped with a debug-level log.
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment>;
}

// ── Pipeline ────────────────────────────────────────────────────────────

/// Ordered collection of context stages.
///
/// ```ignore
/// // Example (requires constructing a ContextBuildContext):
/// use everevo_core::context::{ContextPipeline, SystemPromptStage, ConversationHistoryStage};
/// let pipeline = ContextPipeline::new()
///     .with_stage(SystemPromptStage::default())
///     .with_stage(ConversationHistoryStage { max_messages: 50 });
/// ```
pub struct ContextPipeline {
    stages: Vec<Box<dyn ContextStage>>,
}

impl ContextPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Add a stage. Stages are sorted by priority after insertion.
    pub fn with_stage(mut self, stage: impl ContextStage + 'static) -> Self {
        self.stages.push(Box::new(stage));
        self.stages.sort_by_key(|s| s.priority());
        self
    }

    /// Assemble the full message list AND capture an observability snapshot.
    /// This is the primary entry point for production use.
    pub fn assemble_with_snapshot(
        &self,
        ctx: &ContextBuildContext,
        session_id: uuid::Uuid,
        turn_number: usize,
    ) -> (Vec<LlmMessage>, ContextSnapshot) {
        let mut messages = Vec::new();
        let mut stages = Vec::new();
        let max_budget = ctx.max_context_tokens.max(1);
        let mut flags: Vec<String> = Vec::new();
        let mut total_tokens = 0usize;

        let critical_stages = ["system_prompt", "session_metadata", "latest_message"];

        for stage in &self.stages {
            match stage.build(ctx) {
                Some(fragment) => {
                    let combined: String = fragment
                        .messages
                        .iter()
                        .map(|m| m.content.as_str())
                        .collect::<Vec<&str>>()
                        .join("\n");
                    let msg_count = fragment.messages.len();
                    let tokens = estimate_tokens(&combined);
                    total_tokens += tokens;
                    let preview = truncate_content(&combined, CONTEXT_PREVIEW_MAX_CHARS);

                    // Auto-flag: oversized stage (>40% of budget)
                    let budget_pct = (tokens as f64) / (max_budget as f64) * 100.0;
                    let status = if budget_pct > 40.0 {
                        flags.push(format!(
                            "Stage '{}' uses {:.0}% of context budget (~{} tokens)",
                            stage.name(),
                            budget_pct,
                            tokens
                        ));
                        "oversized"
                    } else {
                        "ok"
                    };

                    tracing::debug!(
                        stage = stage.name(),
                        label = %fragment.label,
                        count = msg_count,
                        estimated_tokens = tokens,
                        "Context stage contributed"
                    );
                    messages.extend(fragment.messages);
                    stages.push(StageSnapshot {
                        stage_name: stage.name().to_string(),
                        priority: stage.priority(),
                        contributed: true,
                        label: Some(fragment.label.clone()),
                        message_count: msg_count,
                        content_preview: Some(preview),
                        estimated_tokens: tokens,
                        status: status.to_string(),
                    });
                }
                None => {
                    tracing::trace!(
                        stage = stage.name(),
                        "Context stage skipped (no contribution)"
                    );

                    // Auto-flag: critical stages that should never be missing
                    let is_critical = critical_stages.contains(&stage.name());
                    if is_critical {
                        flags.push(format!(
                            "Critical stage '{}' returned no content — check configuration",
                            stage.name()
                        ));
                    }

                    stages.push(StageSnapshot {
                        stage_name: stage.name().to_string(),
                        priority: stage.priority(),
                        contributed: false,
                        label: None,
                        message_count: 0,
                        content_preview: None,
                        estimated_tokens: 0,
                        status: if is_critical {
                            "missing".to_string()
                        } else {
                            "warn".to_string()
                        },
                    });
                }
            }
        }

        let budget_used_pct = (total_tokens as f64) / (max_budget as f64) * 100.0;
        if budget_used_pct > 100.0 {
            flags.push(format!(
                "Context budget exceeded: {:.0}% of {} max tokens",
                budget_used_pct, max_budget
            ));
        }

        let snapshot = ContextSnapshot {
            session_id,
            turn_number,
            captured_at: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
            stages,
            total_estimated_tokens: total_tokens,
            max_context_tokens: max_budget,
            budget_used_pct,
            flags,
        };

        (messages, snapshot)
    }

    /// Assemble messages only — convenience wrapper for tests and legacy callers.
    pub fn assemble(&self, ctx: &ContextBuildContext) -> Vec<LlmMessage> {
        self.assemble_with_snapshot(ctx, uuid::Uuid::nil(), 0).0
    }
}

impl Default for ContextPipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in Stages ─────────────────────────────────────────────────────

/// Injects the system prompt as the first message.
pub struct SystemPromptStage {
    pub prompt: String,
}

impl SystemPromptStage {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
        }
    }
}

impl ContextStage for SystemPromptStage {
    fn priority(&self) -> i32 {
        0
    }
    fn name(&self) -> &str {
        "system_prompt"
    }
    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        if self.prompt.is_empty() {
            return None;
        }
        Some(ContextFragment {
            label: "System Prompt".into(),
            messages: vec![LlmMessage::system(&self.prompt)],
        })
    }
}

/// Injects current-session conversation history with a sliding-window cap.
pub struct ConversationHistoryStage {
    /// Maximum number of past messages to include (oldest are dropped first).
    pub max_messages: usize,
}

impl Default for ConversationHistoryStage {
    fn default() -> Self {
        Self { max_messages: 50 }
    }
}

impl ContextStage for ConversationHistoryStage {
    fn priority(&self) -> i32 {
        80
    }
    fn name(&self) -> &str {
        "conversation_history"
    }
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        if ctx.history.is_empty() {
            return None;
        }
        // Apply sliding window: keep the most recent N messages
        let window = if ctx.history.len() > self.max_messages {
            &ctx.history[ctx.history.len() - self.max_messages..]
        } else {
            &ctx.history
        };
        Some(ContextFragment {
            label: format!("History ({} messages)", window.len()),
            messages: window.to_vec(),
        })
    }
}

/// Injects the latest user message (always last).
pub struct LatestMessageStage;

impl ContextStage for LatestMessageStage {
    fn priority(&self) -> i32 {
        90
    }
    fn name(&self) -> &str {
        "latest_message"
    }
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        if ctx.user_message.is_empty() {
            return None;
        }
        Some(ContextFragment {
            label: "User Input".into(),
            messages: vec![LlmMessage::user(&ctx.user_message)],
        })
    }
}

/// Injects runtime environment info — shell, permissions, trusted paths.
/// Claude Code equivalent: session metadata injected before each API call.
pub struct SessionMetadataStage;

impl ContextStage for SessionMetadataStage {
    fn priority(&self) -> i32 {
        5 // right after system prompt (0), before history (80)
    }
    fn name(&self) -> &str {
        "session_metadata"
    }
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let shell = ctx.shell_name.as_deref().unwrap_or("unknown");
        let perm = ctx.permission_level.as_deref().unwrap_or("unknown");

        // Shell-specific command syntax guidance
        let shell_guide = shell_specific_guide(shell);

        // Build workspace line
        let workspace_line = if let Some(ref wp) = ctx.workspace_path {
            format!("Working directory: {wp}\n")
        } else {
            String::new()
        };

        // Current date (Claude Code alignment)
        let today = ctx
            .current_date
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
        let date_line = format!("Today's date is {today}.\n");

        // Git info (Claude Code alignment)
        let git_lines = if let Some(ref branch) = ctx.git_branch {
            let status_line = ctx
                .git_status
                .as_ref()
                .map(|s| format!("Git status: {s}\n"))
                .unwrap_or_default();
            format!("Is a git repository: true\nGit branch: {branch}\n{status_line}")
        } else {
            String::new()
        };

        // Workspace context files (CLAUDE.md / AGENTS.md discovery)
        let context_files_block = if !ctx.workspace_context_files.is_empty() {
            let mut block = String::from("\n## Project Context\n\n");
            for (path, content) in &ctx.workspace_context_files {
                // Truncate very long context files to avoid blowing context budget
                let truncated = if content.chars().count() > 4000 {
                    let safe: String = content.chars().take(4000).collect();
                    format!(
                        "{safe}...\n[truncated — full file at {}]",
                        path
                    )
                } else {
                    content.clone()
                };
                block.push_str(&format!(
                    "### {} \n\n{truncated}\n\n",
                    path.rsplit('/').next().unwrap_or(path)
                ));
            }
            block
        } else {
            String::new()
        };

        // ── Environment (single reality — no host/sandbox distinction) ──
        let runtime_info = ctx.runtime_summary.as_deref().unwrap_or("available");
        let startup_note = if ctx.startup_verified {
            "✅ system startup checks passed"
        } else {
            "⚠️ some startup checks had warnings (non-critical)"
        };

        let mut info = format!(
            "{context_files_block}\
             ## Environment\n\n\
             {date_line}\
             OS: {os}\n\
             Shell: {shell}\n\
             {workspace_line}\
             {git_lines}\
             **Runtimes**: {runtime_info}\n\
             **Status**: {startup_note}\n\
             {tools} tools, permission level: {perm}\n\n\
             {shell_guide}\n\
             ## Permissions\n\n\
             - Working directory and its children: free access.\n\
             - Other paths: require user approval (a confirmation dialog).\n\
             - Dangerous operations require confirmation regardless of path.\n\
             - If something is denied: explain why and suggest an alternative.\n\
             - If a tool or runtime is missing: use `which <name>` to check, \
             then tell the user what's needed.\n",
            shell = shell,
            perm = perm,
            tools = ctx.tool_count,
            date_line = date_line,
            workspace_line = workspace_line,
            os = ctx.platform.as_deref().unwrap_or(std::env::consts::OS),
            git_lines = git_lines,
            shell_guide = shell_guide,
            context_files_block = context_files_block,
            runtime_info = runtime_info,
            startup_note = startup_note,
        );
        if !ctx.trusted_paths.is_empty() {
            info.push_str(&format!(
                "\nTrusted write paths (no confirmation needed): {}\n",
                ctx.trusted_paths.join(", ")
            ));
        }

        Some(ContextFragment {
            label: "Runtime + Permissions".into(),
            messages: vec![LlmMessage::user(&info)],
        })
    }
}

/// Injects the current TodoWrite task state so the agent knows what's
/// pending vs completed. This is critical for correctly interpreting
/// "继续" (continue) — the agent must resume pending work, not redo
/// completed work that was most recently discussed.
///
/// Priority 3: runs after BestPractices (2), before SessionMetadata (5).
pub struct TaskStateStage;

impl ContextStage for TaskStateStage {
    fn priority(&self) -> i32 {
        3
    }
    fn name(&self) -> &str {
        "task_state"
    }
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let summary = ctx.todo_summary.as_deref().unwrap_or("");
        if summary.is_empty() {
            // No task list — inject a lightweight reminder about "继续" interpretation
            return Some(ContextFragment {
                label: "Task State (empty)".into(),
                messages: vec![LlmMessage::user(
                    "## Task State\n\n\
                     No task list is currently tracked. If the user says \"继续\" (continue) or \
                     mentions they've completed something:\n\
                     - Check the conversation history for what was in progress.\n\
                     - If the user says they did something, VERIFY it — don't redo it.\n\
                     - If continuing, resume the oldest UNFINISHED work, not the most recently \
                     discussed topic.\n",
                )],
            });
        }
        Some(ContextFragment {
            label: "Task State".into(),
            messages: vec![LlmMessage::user(format!(
                "## Current Task State\n\n\
                 {summary}\n\n\
                 When the user says \"继续\" (continue): resume the oldest pending/in_progress \
                 task — NOT the most recently discussed topic and NOT anything marked completed.\n\
                 When the user says they've done something: verify it; do NOT redo it.\n",
            ))],
        })
    }
}

/// Return shell-specific command syntax guidance so the LLM uses
/// the correct path separators, commands, and quoting for each shell.
pub fn shell_specific_guide(shell_name: &str) -> &'static str {
    match shell_name {
        "Git Bash" | "Git Bash (PATH)" => {
            "\
## Shell: Git Bash (Unix-like on Windows)\n\
- Commands: Unix-style — use `ls`, `cat`, `grep`, `rm`, `cp`, `mv`\n\
- Paths: use FORWARD slashes ONLY — `F:/Users/lcx/Desktop/file.txt`\n\
- NEVER use backslashes in paths — `\\` is an escape character in bash!\n\
- Wrong: `F:\\Users\\Desktop\\file.txt` → broken\n\
- Correct: `F:/Users/lcx/Desktop/file.txt` or `/f/Users/lcx/Desktop/file.txt`\n\
- File content: use `cat file.txt` NOT `type file.txt`\n\
- Directory listing: use `ls -la` NOT `dir`\n\
- Python: use `python` (not `python3`)"
        }

        "WSL" => {
            "\
## Shell: WSL (Linux subsystem on Windows)\n\
- You are running INSIDE a WSL2 Linux virtual machine, NOT on Windows directly.\n\
- Commands: standard Linux — `ls`, `cat`, `grep`, `rm`, `cp`, `mv`\n\
- Paths: Linux-style ONLY — `/home/user/...`, `/mnt/c/Users/...`, `/mnt/f/...`\n\
- Windows drives are at `/mnt/c/`, `/mnt/d/`, `/mnt/f/` — NOT `C:\\`, `F:\\`\n\
- Python: use `python3` (or `python` if configured)\n\
- Note: You are NOT in Docker. Docker Desktop runs in a SEPARATE WSL2 VM.\n\
  The `docker` CLI works because it connects to dockerd via a socket,\n\
  but your working directory and files are on the WSL filesystem."
        }

        "PowerShell" => {
            "\
## Shell: PowerShell (Windows)\n\
- Commands: PowerShell cmdlets — `Get-ChildItem`, `Get-Content`, `Set-Location`\n\
- Aliases: `ls`, `cat`, `cd`, `rm` work as aliases\n\
- Paths: Windows-style — `C:\\Users\\...`, `F:\\Desktop\\...`\n\
- Use backticks `` ` `` for escaping, NOT backslash\n\
- File content: `Get-Content file.txt` or `cat file.txt`\n\
- Directory listing: `Get-ChildItem` or `ls`"
        }

        "CMD" => {
            "\
## Shell: CMD (Windows Command Prompt)\n\
- Commands: Windows — `dir`, `type`, `del`, `copy`, `move`\n\
- Paths: Windows-style — `C:\\Users\\...`, `F:\\Desktop\\...`\n\
- File content: `type file.txt`\n\
- Directory listing: `dir`\n\
- Environment variables: `%VAR%`"
        }

        _ => {
            "\
## Shell: Unknown\n\
- Use standard shell commands appropriate for your platform\n\
- Prefer forward-slash paths when uncertain"
        }
    }
}

// ── Builder Convenience ─────────────────────────────────────────────────

/// Build a default production pipeline with the standard stages.
///
/// Callers can add custom stages (RAG, KG, tools) via `with_stage()`.
pub fn default_pipeline() -> ContextPipeline {
    ContextPipeline::new()
        .with_stage(SystemPromptStage::new(SYSTEM_PROMPT))
        .with_stage(TaskStateStage)
        .with_stage(SessionMetadataStage)
        .with_stage(ConversationHistoryStage::default())
        .with_stage(LatestMessageStage)
}

/// Default system prompt — shell-specific instructions are injected
/// dynamically by SessionMetadataStage, so this stays shell-agnostic.
pub const SYSTEM_PROMPT: &str = "\
You are EverEvo, a desktop AI agent. Use tools to DO things — never just describe.\n\
\n\
## Tool Rules (MUST FOLLOW)\n\
\n\
Shell is LAST RESORT. Use specialized tools first:\n\
\n\
| Operation | ✅ | ❌ |\n\
|-----------|---|---|\n\
| Read file | `read_file` | `shell cat` |\n\
| Write file | `write_file` | `shell echo` |\n\
| List dir | `list_dir` | `shell ls` |\n\
| Search code | `code_search` | `shell grep` |\n\
| Search web | `web_search` | `shell curl` |\n\
| Fetch URL | `web_fetch` | `shell curl` |\n\
| Download | `download` | `shell wget` |\n\
| Build/test/run | `shell` | — (OK) |\n\
| Git/packages | `shell` | — (OK) |\n\
\n\
Other tools: `TodoWrite` (tasks, scope=session/global), `Task` (sub-agents) + \
`cancel_task` (stop one by id), `team`/`cluster`/`parallel_agents` (multi-agent), \
`memory`, `list_workflows` + `workflow_run` (reusable automations, run by name), \
`EnterPlanMode`/`ExitPlanMode`, `Verify`, `Skill`, `code_map`, `compact`, \
`bootstrap_check`, MCP tools.\n\
\n\
## When to Delegate / Collaborate\n\
\n\
| Situation | Use |\n\
|-----------|-----|\n\
| 2+ independent sub-tasks | `Task` with `subtasks` (parallel) |\n\
| Focused reasoning in isolation | `Task` (single sub-agent) |\n\
| Role-based review/research/coding | `team` |\n\
| Adversarial verify (majority vote) | `cluster` (verify) |\n\
| Map->reduce over many items | `cluster` (map_reduce) |\n\
| Repeatable multi-step procedure | `list_workflows` -> `workflow_run name=` |\n\
| Sub-agent gone wrong / too slow | `cancel_task <task_id>` |\n\
\n\
Don't delegate trivial single-step lookups - just call the tool. The `task` tool \
returns a task_id; use `cancel_task` to stop it. Prefer a saved workflow (by name) \
over hand-writing steps. Use `TodoWrite` with scope=global for project work that \
spans conversations.\n\
\n\
## Self-Evolution (learn from every task)\n\
\n\
- After each task, lessons + repeatable procedures are auto-saved (memory + workflows).\n\
- BEFORE a non-trivial task: run `list_workflows` and check memory — REUSE before re-inventing.\n\
- Found a matching workflow? `workflow_run name=` instead of hand-rolling steps.\n\
- Solved a repeatable multi-step problem? `save_workflow` it for next time.\n\
- Sedimented lessons auto-surface in future turns — trust and build on them.\n\
\n\
## Critical Rules\n\
\n\
- **2-failure limit**: If a command fails twice, STOP. Diagnose root cause \
(`which`, `echo $VAR`, read error), web_search the error, switch approach \
(SSH→HTTPS, different library). Never retry with minor tweaks.\n\
- **\"我做了X\" = report, not request**: User stating completion → VERIFY, don't redo.\n\
- **\"继续\" = resume oldest PENDING TodoWrite task**, not most recent topic.\n\
- **SSH→HTTPS**: Use `git clone https://...` and `gh` CLI. Never `git@github.com:`.\n\
- **Git auth**: Uses your global git config and SSH/HTTPS settings. \
  `gh` CLI uses stored OAuth. No extra credential setup needed.\n\
- **[SYSTEM NOTE] / [REQUIRED] messages**: Follow them — they're not suggestions.\n\
- **Type `/help`** to see slash commands. `/clear` resets context. `/compact` saves space.\n\
- **Admit when stuck**: \"I tried X, Y, Z. Here's what failed and what I need.\" \
Better than looping.\n\
- Verify before claiming done. Fix code, never weaken tests. Match existing style.";

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmRole;

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

    // ── ContextPipeline ────────────────────────────────────────────────

    #[test]
    fn test_pipeline_new_is_empty() {
        let pipeline = ContextPipeline::new();
        let ctx = ContextBuildContext::default();
        let (messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);
        assert!(messages.is_empty());
        assert_eq!(snapshot.total_estimated_tokens, 0);
    }

    #[test]
    fn test_pipeline_stages_sorted_by_priority() {
        let pipeline = ContextPipeline::new()
            .with_stage(LatestMessageStage)
            .with_stage(SystemPromptStage::new("test"))
            .with_stage(ConversationHistoryStage::default());

        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "hello".into();
        ctx.history = vec![LlmMessage::user("old")];

        let (_messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);

        assert_eq!(snapshot.stages.len(), 3);
        assert_eq!(snapshot.stages[0].stage_name, "system_prompt");
        assert_eq!(snapshot.stages[1].stage_name, "conversation_history");
        assert_eq!(snapshot.stages[2].stage_name, "latest_message");
    }

    // ── SystemPromptStage ──────────────────────────────────────────────

    #[test]
    fn test_system_prompt_stage_builds() {
        let stage = SystemPromptStage::new("You are helpful.");
        let ctx = ContextBuildContext::default();
        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.messages[0].role, LlmRole::System);
        assert_eq!(fragment.messages[0].content, "You are helpful.");
    }

    #[test]
    fn test_system_prompt_stage_empty_returns_none() {
        let stage = SystemPromptStage::new("");
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }

    // ── LatestMessageStage ─────────────────────────────────────────────

    #[test]
    fn test_latest_message_builds() {
        let stage = LatestMessageStage;
        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "hello".into();
        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.messages[0].content, "hello");
    }

    #[test]
    fn test_latest_message_empty_returns_none() {
        let stage = LatestMessageStage;
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }

    // ── ConversationHistoryStage ───────────────────────────────────────

    #[test]
    fn test_history_empty_returns_none() {
        let stage = ConversationHistoryStage::default();
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }

    #[test]
    fn test_history_builds() {
        let stage = ConversationHistoryStage::default();
        let mut ctx = ContextBuildContext::default();
        ctx.history = vec![LlmMessage::user("q1"), LlmMessage::assistant("a1")];
        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.messages.len(), 2);
    }

    #[test]
    fn test_history_sliding_window() {
        let stage = ConversationHistoryStage { max_messages: 2 };
        let mut ctx = ContextBuildContext::default();
        ctx.history = vec![
            LlmMessage::user("old1"),
            LlmMessage::assistant("old2"),
            LlmMessage::user("new1"),
            LlmMessage::assistant("new2"),
        ];
        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.messages.len(), 2);
        assert_eq!(fragment.messages[0].content, "new1");
    }

    // ── SessionMetadataStage ───────────────────────────────────────────

    #[test]
    fn test_session_metadata_always_builds() {
        let stage = SessionMetadataStage;
        let ctx = ContextBuildContext::default();
        let fragment = stage.build(&ctx).unwrap();
        assert!(fragment.messages[0].content.contains("Environment"));
        assert!(fragment.messages[0].content.contains("Permissions"));
    }

    #[test]
    fn test_session_metadata_includes_workspace() {
        let stage = SessionMetadataStage;
        let mut ctx = ContextBuildContext::default();
        ctx.workspace_path = Some("/home/user".into());
        let fragment = stage.build(&ctx).unwrap();
        assert!(fragment.messages[0].content.contains("/home/user"));
    }

    #[test]
    fn test_session_metadata_includes_git() {
        let stage = SessionMetadataStage;
        let mut ctx = ContextBuildContext::default();
        ctx.git_branch = Some("main".into());
        ctx.git_status = Some("clean".into());
        let fragment = stage.build(&ctx).unwrap();
        assert!(fragment.messages[0].content.contains("main"));
        assert!(fragment.messages[0].content.contains("clean"));
    }

    #[test]
    fn test_session_metadata_includes_trusted() {
        let stage = SessionMetadataStage;
        let mut ctx = ContextBuildContext::default();
        ctx.trusted_paths = vec!["/safe".into()];
        let fragment = stage.build(&ctx).unwrap();
        assert!(fragment.messages[0].content.contains("/safe"));
    }

    // ── TaskStateStage ─────────────────────────────────────────────────

    #[test]
    fn test_task_state_with_todo() {
        let stage = TaskStateStage;
        let mut ctx = ContextBuildContext::default();
        ctx.todo_summary = Some("[ ] pending task".into());
        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.label, "Task State");
        assert!(fragment.messages[0].content.contains("pending task"));
    }

    #[test]
    fn test_task_state_empty_gives_reminder() {
        let stage = TaskStateStage;
        let ctx = ContextBuildContext::default();
        let fragment = stage.build(&ctx).unwrap();
        assert!(fragment.messages[0].content.contains("继续"));
    }

    // ── shell_specific_guide ───────────────────────────────────────────

    #[test]
    fn test_shell_guide_git_bash() {
        let guide = shell_specific_guide("Git Bash");
        assert!(guide.contains("FORWARD slashes"));
    }

    #[test]
    fn test_shell_guide_powershell() {
        let guide = shell_specific_guide("PowerShell");
        assert!(guide.contains("Get-ChildItem"));
    }

    #[test]
    fn test_shell_guide_wsl() {
        let guide = shell_specific_guide("WSL");
        assert!(guide.contains("WSL2"));
    }

    #[test]
    fn test_shell_guide_cmd() {
        let guide = shell_specific_guide("CMD");
        assert!(guide.contains("Command Prompt"));
    }

    #[test]
    fn test_shell_guide_unknown() {
        let guide = shell_specific_guide("zsh");
        assert!(guide.contains("Unknown"));
    }

    // ── default_pipeline ───────────────────────────────────────────────

    #[test]
    fn test_default_pipeline_produces_output() {
        let pipeline = default_pipeline();
        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "hello".into();
        let (messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);
        assert!(!snapshot.stages.is_empty());
        assert!(!messages.is_empty());
    }

    // ── assemble_with_snapshot: observability flags ────────────────────

    #[test]
    fn test_critical_stage_missing_is_flagged() {
        let pipeline = ContextPipeline::new()
            .with_stage(SystemPromptStage::new(""))
            .with_stage(LatestMessageStage);

        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "hi".into();
        let (_messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);

        assert!(snapshot
            .flags
            .iter()
            .any(|f| f.contains("system_prompt") && f.contains("no content")));

        let sys = snapshot
            .stages
            .iter()
            .find(|s| s.stage_name == "system_prompt")
            .unwrap();
        assert!(!sys.contributed);
        assert_eq!(sys.status, "missing");
    }

    #[test]
    fn test_oversized_stage_is_flagged() {
        struct HugeStage;
        impl ContextStage for HugeStage {
            fn priority(&self) -> i32 {
                5
            }
            fn name(&self) -> &str {
                "huge"
            }
            fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
                let content = "x".repeat(500 * 4);
                Some(ContextFragment {
                    label: "huge".into(),
                    messages: vec![LlmMessage::user(&content)],
                })
            }
        }

        let pipeline = ContextPipeline::new().with_stage(HugeStage);
        let mut ctx = ContextBuildContext::default();
        ctx.max_context_tokens = 1000;

        let (_messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);

        let huge = snapshot
            .stages
            .iter()
            .find(|s| s.stage_name == "huge")
            .unwrap();
        assert_eq!(huge.status, "oversized");
    }

    #[test]
    fn test_budget_exceeded_flag() {
        struct MassiveStage;
        impl ContextStage for MassiveStage {
            fn priority(&self) -> i32 {
                1
            }
            fn name(&self) -> &str {
                "massive"
            }
            fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
                let content = "x".repeat(5000 * 4);
                Some(ContextFragment {
                    label: "massive".into(),
                    messages: vec![LlmMessage::user(&content)],
                })
            }
        }

        let pipeline = ContextPipeline::new().with_stage(MassiveStage);
        let mut ctx = ContextBuildContext::default();
        ctx.max_context_tokens = 500;

        let (_messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::new_v4(), 1);

        assert!(snapshot
            .flags
            .iter()
            .any(|f| f.contains("budget") && f.contains("exceeded")));
        assert!(snapshot.budget_used_pct > 100.0);
    }

    #[test]
    fn test_snapshot_metadata_correct() {
        let sid = uuid::Uuid::new_v4();
        let pipeline = ContextPipeline::new().with_stage(LatestMessageStage);
        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "hi".into();
        ctx.max_context_tokens = 8000;

        let (_messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, sid, 42);

        assert_eq!(snapshot.session_id, sid);
        assert_eq!(snapshot.turn_number, 42);
        assert_eq!(snapshot.max_context_tokens, 8000);
        assert!(!snapshot.captured_at.is_empty());
    }

    #[test]
    fn test_assemble_legacy_delegates() {
        let pipeline = ContextPipeline::new().with_stage(LatestMessageStage);
        let mut ctx = ContextBuildContext::default();
        ctx.user_message = "test".into();
        let messages = pipeline.assemble(&ctx);
        assert_eq!(messages.len(), 1);
    }
}
