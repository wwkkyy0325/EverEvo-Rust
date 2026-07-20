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
                    tracing::trace!(stage = stage.name(), "Context stage skipped (no contribution)");
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

        let mut info = format!(
            "## Runtime Environment\n\
             Shell: {shell}\n\
             Permission: {perm} (semi_auto = dangerous commands require your confirmation)\n\
             {tools} tools registered\n\
             Working directory: sandbox work dir\n\n\
             {shell_guide}\n\
             ## Permission Rules\n\
             - Use RELATIVE paths (./file.txt) for files inside the sandbox\n\
             - External paths (outside sandbox) trigger a user confirmation dialog\n\
             - Dangerous commands (rm -rf, curl|bash, chmod +s, nmap, etc.) trigger confirmation\n\
             - Admin commands (sudo, runas) ALWAYS require user approval\n\
             - The user will see a popup — explain what you're doing and they'll approve\n\
             - If a command is denied: explain why and suggest an alternative",
            shell = shell,
            perm = perm,
            tools = ctx.tool_count,
            shell_guide = shell_guide,
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

/// Return shell-specific command syntax guidance so the LLM uses
/// the correct path separators, commands, and quoting for each shell.
pub fn shell_specific_guide(shell_name: &str) -> &'static str {
    match shell_name {
        "Git Bash" | "Git Bash (PATH)" => "\
## Shell: Git Bash (Unix-like on Windows)\n\
- Commands: Unix-style — use `ls`, `cat`, `grep`, `rm`, `cp`, `mv`\n\
- Paths: use FORWARD slashes ONLY — `F:/Users/lcx/Desktop/file.txt`\n\
- NEVER use backslashes in paths — `\\` is an escape character in bash!\n\
- Wrong: `F:\\Users\\Desktop\\file.txt` → broken\n\
- Correct: `F:/Users/lcx/Desktop/file.txt` or `/f/Users/lcx/Desktop/file.txt`\n\
- File content: use `cat file.txt` NOT `type file.txt`\n\
- Directory listing: use `ls -la` NOT `dir`\n\
- Python: use `python` (not `python3`)",

        "WSL" => "\
## Shell: WSL (Linux on Windows)\n\
- Commands: standard Linux — `ls`, `cat`, `grep`, `rm`, `cp`, `mv`\n\
- Paths: Linux-style — `/home/user/...`, `/mnt/c/Users/...`, `/mnt/f/...`\n\
- Windows drives are at `/mnt/c/`, `/mnt/d/`, `/mnt/f/`, etc.\n\
- Python: use `python3` (or `python` if configured)",

        "PowerShell" => "\
## Shell: PowerShell (Windows)\n\
- Commands: PowerShell cmdlets — `Get-ChildItem`, `Get-Content`, `Set-Location`\n\
- Aliases: `ls`, `cat`, `cd`, `rm` work as aliases\n\
- Paths: Windows-style — `C:\\Users\\...`, `F:\\Desktop\\...`\n\
- Use backticks `` ` `` for escaping, NOT backslash\n\
- File content: `Get-Content file.txt` or `cat file.txt`\n\
- Directory listing: `Get-ChildItem` or `ls`",

        "CMD" => "\
## Shell: CMD (Windows Command Prompt)\n\
- Commands: Windows — `dir`, `type`, `del`, `copy`, `move`\n\
- Paths: Windows-style — `C:\\Users\\...`, `F:\\Desktop\\...`\n\
- File content: `type file.txt`\n\
- Directory listing: `dir`\n\
- Environment variables: `%VAR%`",

        _ => "\
## Shell: Unknown\n\
- Use standard shell commands appropriate for your platform\n\
- Prefer forward-slash paths when uncertain",
    }
}

// ── Builder Convenience ─────────────────────────────────────────────────

/// Build a default production pipeline with the standard stages.
///
/// Callers can add custom stages (RAG, KG, tools) via `with_stage()`.
pub fn default_pipeline() -> ContextPipeline {
    ContextPipeline::new()
        .with_stage(SystemPromptStage::new(SYSTEM_PROMPT))
        .with_stage(SessionMetadataStage)
        .with_stage(ConversationHistoryStage::default())
        .with_stage(LatestMessageStage)
}

/// Default system prompt — shell-specific instructions are injected
/// dynamically by SessionMetadataStage, so this stays shell-agnostic.
pub const SYSTEM_PROMPT: &str = "\
You are EverEvo, a helpful AI assistant running locally on the user's machine. \
You have access to tools for executing shell commands, downloading files, and checking \
runtime environments. When the user asks you to perform tasks that require real system \
access, use the tools to get accurate results. Be concise and direct.\n\
\n\
## Available Tools\n\
\n\
- `shell` — Execute a shell command in a sandboxed environment. Use this to run code, \
  check files, install packages, or perform system operations. Commands run in an isolated \
  workspace. Default timeout 30s, max 300s.\n\
- `download` — Download files from URLs with multi-mirror failover and resume support. \
  Specify url, dest_path, and optionally region (domestic/international/auto).\n\
- `bootstrap_check` — Check the status of portable runtimes (Python, Node.js, Git, ONNX) \
  and local embedding models. Returns which assets are ready, missing, or corrupt.\n\
\n\
## Guidelines\n\
\n\
- For code execution or system operations: use `shell` tool\n\
- For downloading files: use `download` tool\n\
- For checking environment status: use `bootstrap_check` tool\n\
- If a tool returns an error, explain the error to the user and suggest next steps\n\
- **IMPORTANT**: When the user asks you to DO something (run a command, check status, \
  download a file), you MUST use the appropriate tool. Do not just describe what you \
  would do — actually do it.\n\
- All shell commands run in an isolated sandbox with audit logging\n\
- Use the shell-specific path syntax described in the Runtime Environment section below\n\
- For writing files inside the sandbox: use RELATIVE paths (e.g., `./output.txt`)";
