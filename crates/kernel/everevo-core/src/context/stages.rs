use super::budget::{ContextBuildContext, ContextFragment};
use super::ContextStage;
use crate::llm::LlmMessage;

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

/// Injects the durable rolling conversation summary before the history window
/// (spec D3). The summary is maintained incrementally and kept verbatim — it is
/// never re-summarized (rule 1).
pub struct RollingSummaryStage;

impl ContextStage for RollingSummaryStage {
    fn priority(&self) -> i32 {
        75 // between domain knowledge (4+) and conversation history (80)
    }
    fn name(&self) -> &str {
        "rolling_summary"
    }
    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let summary = ctx.summary.as_deref()?;
        if summary.trim().is_empty() {
            return None;
        }
        Some(ContextFragment {
            label: "Rolling Summary".into(),
            messages: vec![LlmMessage::user(format!(
                "<conversation_summary>\n{summary}\n</conversation_summary>"
            ))],
        })
    }
}

/// Injects current-session conversation history with a sliding-window cap.
///
/// When a per-model `ContextBudget` is set (`ctx.budget.window > 0`), the
/// window is token-budget based — newest-first accumulation until
/// `history_budget` is exhausted. Otherwise it falls back to a message-count
/// cap (`max_messages`, kept for backward compatibility / sub-agents).
pub struct ConversationHistoryStage {
    /// Maximum number of past messages to include (oldest are dropped first).
    /// Used only when no token budget is configured.
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
        // Apply sliding window: token-budget newest-first when a budget is set,
        // otherwise the legacy message-count window.
        let window = if ctx.budget.window > 0 {
            ctx.budget.history_window(&ctx.history)
        } else if ctx.history.len() > self.max_messages {
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
                    format!("{safe}...\n[truncated — full file at {}]", path)
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
             then tell the user what's needed.\n\n\
             ## Self-Evolution (Plugin & Skill Modification)\n\n\
             You can evolve your own capabilities by modifying plugins, skills, \
             and workflows:\n\
             - `plugin_dev` — list all plugins, read/edit source, compile new versions\n\
             - `plugin_status` — check plugin versions, manage canary deployments\n\
             - `plugin_rollback` — emergency rollback any plugin to its stable version\n\
             - `skill_compose` / `skill_search` — create and discover reusable skills\n\
             - `workflow_run` / `save_workflow` — execute and sediment repeatable procedures\n\
             Kernel code (crates/kernel/) is IMMUTABLE and protected — you can only \
             modify code under plugins/ and workflows under data/workflows/.\n",
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
- Paths: use FORWARD slashes ONLY — `C:/Users/you/Desktop/file.txt`\n\
- NEVER use backslashes in paths — `\\` is an escape character in bash!\n\
- Wrong: `C:\\Users\\you\\Desktop\\file.txt` → broken\n\
- Correct: `C:/Users/you/Desktop/file.txt` or `/c/Users/you/Desktop/file.txt`\n\
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

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmRole;

    // ── RollingSummaryStage (spec D3) ──────────────────────────────────

    #[test]
    fn rolling_summary_emits_when_summary_present() {
        let mut ctx = ContextBuildContext::default();
        ctx.summary = Some("Atlas migration planned for 2026-09-15.".into());
        let frag = RollingSummaryStage.build(&ctx).expect("stage contributes");
        assert_eq!(frag.messages.len(), 1);
        assert!(frag.messages[0].content.contains("Atlas migration planned"));
        assert!(frag.messages[0].content.contains("<conversation_summary>"));
    }

    #[test]
    fn rolling_summary_none_when_unset() {
        let ctx = ContextBuildContext::default(); // summary = None
        assert!(RollingSummaryStage.build(&ctx).is_none());
    }

    #[test]
    fn rolling_summary_priority_before_history() {
        assert!(RollingSummaryStage.priority() < ConversationHistoryStage::default().priority());
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
}
