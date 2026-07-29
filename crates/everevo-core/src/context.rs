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
    /// Current proactivity escalation level (0-4).
    /// 0 = normal, 1 = hint, 2 = research required,
    /// 3 = forced divergence, 4 = external consult.
    /// Set by AgentLoop after each tool result; read by BestPracticesStage.
    pub escalation_level: Option<u32>,
    /// Detail about the fixation pattern (tool name + error summary),
    /// for targeted nudges. Only set when escalation_level >= 2.
    pub fixation_detail: Option<String>,
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

    /// Assemble the full message list by running every stage in priority order.
    pub fn assemble(&self, ctx: &ContextBuildContext) -> Vec<LlmMessage> {
        let mut messages = Vec::new();

        for stage in &self.stages {
            match stage.build(ctx) {
                Some(fragment) => {
                    tracing::debug!(
                        stage = stage.name(),
                        label = %fragment.label,
                        count = fragment.messages.len(),
                        "Context stage contributed"
                    );
                    messages.extend(fragment.messages);
                }
                None => {
                    tracing::trace!(
                        stage = stage.name(),
                        "Context stage skipped (no contribution)"
                    );
                }
            }
        }

        messages
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

        // Build workspace line — show actual path when set (Claude Code alignment)
        let workspace_line = if let Some(ref wp) = ctx.workspace_path {
            format!("Primary working directory: {wp}\n")
        } else {
            "Working directory: sandbox work dir\n".to_string()
        };
        let work_area = if ctx.workspace_path.is_some() {
            "workspace"
        } else {
            "sandbox"
        };

        // Platform line (Claude Code alignment)
        let platform = ctx.platform.as_deref().unwrap_or(std::env::consts::OS);
        let platform_line = format!("Platform: {platform}\n");

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
                let truncated = if content.len() > 4000 {
                    format!("{}...\n[truncated — full file at {}]", &content[..4000], path)
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

        let mut info = format!(
            "{context_files_block}\
             ## Runtime Environment\n\
             {date_line}\
             Shell: {shell}\n\
             {platform_line}\
             {workspace_line}\
             {git_lines}\
             Permission: {perm} (semi_auto = dangerous commands require your confirmation)\n\
             {tools} tools registered\n\n\
             {shell_guide}\n\
             ## Permission Rules\n\
             - Use RELATIVE paths (./file.txt) for files inside the {work_area}\n\
             - External paths (outside {work_area}) trigger a user confirmation dialog\n\
             - Dangerous commands (rm -rf, curl|bash, chmod +s, nmap, etc.) trigger confirmation\n\
             - Admin commands (sudo, runas) ALWAYS require user approval\n\
             - The user will see a popup — explain what you're doing and they'll approve\n\
             - If a command is denied: explain why and suggest an alternative",
            shell = shell,
            perm = perm,
            tools = ctx.tool_count,
            date_line = date_line,
            workspace_line = workspace_line,
            platform_line = platform_line,
            git_lines = git_lines,
            work_area = work_area,
            shell_guide = shell_guide,
            context_files_block = context_files_block,
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
You are EverEvo, a powerful desktop AI agent running locally on the user's machine. \
You have access to tools for shell execution, file downloads, environment checks, \
and long-term memory. Use tools whenever the user asks you to DO something — \
never just describe what you would do.\n\
\n\
## Available Tools\n\
\n\
- `shell` — Execute a shell command in a sandboxed environment (timeout 30s, max 300s). \
  Use for: running code, checking files, installing packages, system operations.\n\
- `download` — Download files from URLs with multi-mirror failover and resume support. \
  Parameters: url, dest_path, region (domestic/international/auto).\n\
- `bootstrap_check` — Check status of portable runtimes (Python, Node.js, Git, ONNX) \
  and local embedding models. Returns which assets are ready, missing, or corrupt.\n\
- `memory` — Search and manage persistent long-term memory. Use to recall facts, \
  preferences, and past decisions across sessions. Parameters: action (search/save/delete), \
  query, content.\n\
- `TodoWrite` — Create and manage a structured task list for the current session. \
  Use proactively for multi-step tasks. Each task needs content, status \
  (pending/in_progress/completed), and activeForm. Exactly ONE task in_progress \
  at a time. Mark complete immediately after finishing.\n\
- `Workflow` — Execute multiple tasks in parallel using sub-agents. Use for complex \
  multi-step work where tasks are independent. Provide a list of tasks each with \
  description and prompt. Supports parallel and sequential modes.\n\
- `EnterPlanMode` / `ExitPlanMode` — Use before non-trivial implementation tasks. \
  EnterPlanMode signals you want to plan first; ExitPlanMode submits the plan \
  for user approval before implementation.\n\
- `Skill` — Invoke specialized skills by name. Use action='list' to discover \
  available skills, or provide a skill name to load its instructions. Skills \
  extend capabilities for specific domains (e.g., frontend-design, testing).\n\
- `Verify` — Verify the output of a previous task. Checks for correctness, \
  completeness, and edge cases. Use after sub-agent tasks complete to ensure \
  quality.\n\
- `Task` — Spawn a sub-agent to execute a task independently. Use for complex \
  work that benefits from isolated execution. Sub-agents run with their own \
  tool access and context.\n\
- `web_fetch` — Fetch content from a URL. Strips HTML tags and returns plain text. \
  Use for reading documentation, API docs, or any public webpage. For authenticated \
  URLs, use `shell` with curl instead. Parameters: url.\n\
- `web_search` — Search the web and return result blocks with titles and URLs. \
  Use for finding documentation, error solutions, library docs, or any information \
  that requires up-to-date web knowledge. Prefer this over guessing. \
  Parameters: query (required), limit (default 8, max 20), \
  allowed_domains (optional string array), blocked_domains (optional string array).\n\
- `compact` — Manually trigger context compaction to free up space. \
  Use when the conversation is getting long, you notice context quality degrading, \
  or after a context overflow error. Parameters: focus (optional priority topic).\n\
- `team` — Dispatch a team of role-specialized sub-agents to work in parallel. \
  Roles: reviewer (code review), researcher (investigation), coder (implementation), \
  tester (testing). Each member gets a role-specific system prompt. \
  Parameters: task, members[{role, focus}].\n\
- `code_search` — Search the codebase for symbols (functions, structs, impls, etc.) \
  using a pre-built FTS5 code index. Returns file:line locations with signatures. \
  Parameters: query, kind (optional: fn/struct/impl/trait/enum/mod/type/const), limit. \
  For full-text grep, use the `shell` tool with `rg` or `grep` instead.\n\
- `code_map` — Return a lightweight Markdown directory overview of the codebase. \
  Shows folder structure with one-line descriptions from README/Cargo.toml. \
  Use to understand project layout before diving into specific directories.\n\
- `list_dir` — List files and directories in the workspace. Returns structured \
  output with names, 📁/📄 icons, sizes, and modification times. Use to explore \
  the project structure before reading or editing files. \
  Parameters: path (default: '.'), depth (1-3), limit (default: 50, max: 200).\n\
- `read_file` — Read a file from the workspace. Returns content with line numbers. \
  Use for inspecting source code, config files, or any text file. \
  Parameters: path (required, relative to workspace), offset (1-based line), \
  limit (max lines, default: 2000).\n\
- `write_file` — Create or overwrite a file in the workspace. Creates parent \
  directories automatically. Use for writing code, config, or any text content. \
  Parameters: path (required, relative to workspace), content (required).\n\
- `cluster` — Orchestrate parallel sub-agents using cluster patterns:\n\
  fan_out (N workers on same task), map_reduce (N workers → synthesize), \
  verify (adversarial verification with majority vote). \
  Use for complex tasks that benefit from parallel analysis, diverse perspectives, \
  or quality verification. \
  Parameters: action (fan_out/map_reduce/verify), prompt, \
  workers (default 3, max 10), items/perspectives/claims (action-specific).\n\
- `workflow_run` — Execute a multi-step automation workflow. Define steps as JSON: \
  shell commands, URL fetches, memory operations, sub-agents, delays, conditions, \
  and variable passing between steps. Parameters: workflow (JSON definition).\n\
- MCP tools — Additional tools from connected MCP servers (e.g., web search, \
  file system access). Available when configured in data/config.toml.\n\
\n\
## Docker Safety\n\
\n\
Docker commands run through the `shell` tool. Dangerous operations trigger \
user confirmation before execution:\n\
- **Privileged containers**: --privileged, --pid=host, --network=host, SYS_ADMIN cap\n\
- **Security bypass**: --security-opt label=disable, apparmor=unconfined, seccomp=unconfined\n\
- **Host mounts**: -v /:/host, --device=/dev/*\n\
- **Destructive**: docker rm -f, system prune, volume prune, compose down -v\n\
\n\
## Conversation Continuity\n\
\n\
These rules are CRITICAL for correct behavior across turns:\n\
\n\
- If the user says they've already done something (\"I fixed X\", \"做好了Y\"), \
  they are REPORTING completion. VERIFY — do NOT redo it.\n\
- \"继续\" (continue) means: resume the oldest PENDING task from the TodoWrite list. \
  It does NOT mean redo the most recently discussed topic.\n\
- If no TodoWrite list exists, scan the conversation history to find what was \
  in progress BEFORE the most recent user message.\n\
- Always distinguish: \"I did X\" (verify) vs \"Do X\" (execute) vs \"继续\" (resume pending).\n\
- Never repeat work the user explicitly states they completed.\n\
\n\
## Guidelines\n\
\n\
- Use tools proactively — don't describe what you would do, actually do it.\n\
- When a tool returns an error, explain it and suggest next steps.\n\
- Shell commands run in an isolated sandbox; use RELATIVE paths (e.g., `./output.txt`).\n\
- Be thorough: if a task requires multiple tools, call them in sequence.\n\
- You may use `memory` to store important facts the user might need later.\n\
- When generating plans or multi-step work, break it into clear, numbered steps.\n\
- For web searches (finding information), use `web_search` — it returns result blocks.\n\
- For reading a specific URL, use `web_fetch`; for auth-required URLs, use `shell` + curl.\n\
- Search before fetching — don't guess URLs when `web_search` can find them.\n\
- Use `list_dir` to explore the workspace before reading or creating files.\n\
- Use RELATIVE paths (./file.txt) for files inside the workspace.";
