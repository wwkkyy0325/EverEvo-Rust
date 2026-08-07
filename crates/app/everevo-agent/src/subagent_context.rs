//! Sub-agent context assembly — always reports every stage result, even empty ones.
//!
//! ## Design Principle (Negative Results ARE Results)
//!
//! Claude Code, OpenAI Agents SDK, and CrewAI all distinguish between
//! "context was not searched" and "context was searched but nothing found".
//! A sub-agent must know that retrieval ran — otherwise it wastes turns
//! re-searching for things that don't exist.
//!
//! ## Contrast with Main Agent Pipeline
//!
//! The main agent (`ContextPipeline`) silently skips stages that return
//! `None`. This is fine for a chat interface where the LLM can ask the
//! user for clarification. A sub-agent has no user to ask — it must
//! receive ALL available information on its first turn.
//!
//! ## Stage Execution Order (priority-based)
//!
//! ```text
//! [0] System Prompt        — locked: tools, environment, rules
//! [1] Persona              — communication style (if configured)
//! [2] Skills               — available skill list (if any)
//! [3] Memory Facts         — top-K relevant facts (or "none found")
//! [4] Domain Knowledge     — relevant document chunks (or "not found")
//! [5] Parent Workspace     — path to main agent's work directory
//! [6] Delegation Context   — what the main agent explicitly passed
//! [7] Task Description     — the actual task to execute
//! ```
//!
//! ## References
//!
//! - OpenAI Agents SDK: `AgentContext` with explicit `Memory: Option<Vec<Item>>`
//! - Claude Code: sub-agents receive `## Available Context` section
//! - CrewAI: `Task.context: List[TaskContext]` with mandatory empty-list reporting

use std::path::PathBuf;

use everevo_core::context::{ContextBuildContext, ContextStage};

use crate::stages::DomainKnowledgeStage;
use crate::stages::MemoryStage;

// ── Sub-agent context builder ──────────────────────────────────────────

/// Context injected into every sub-agent. Always built from all available
/// sources; empty results are explicitly reported.
#[derive(Debug, Clone)]
pub struct SubAgentContext {
    /// Locked system information — shell, OS, paths, tool list.
    pub system_info: String,
    /// Persona / communication style (or "(not configured)").
    pub persona: String,
    /// Available skills (or "(no skills registered)").
    pub skills: String,
    /// Relevant memory facts (or "(no relevant memory facts found)").
    pub memory_facts: String,
    /// Relevant domain knowledge (or "(no domain knowledge matched)").
    pub domain_knowledge: String,
    /// Path to parent agent's workspace.
    pub parent_workspace: Option<PathBuf>,
    /// Explicit context from the main agent.
    pub delegation_note: Option<String>,
    /// Parent session's permission level for inheritance.
    /// When FullyAuto, sub-agent shell commands should auto-approve.
    pub permission_level: Option<String>,
    /// Recursion depth (0 = main agent, 1 = sub-agent, 2 = sub-sub-agent, etc.).
    /// Used to prevent infinite recursive delegation. Max depth = 3.
    pub depth: u32,
    /// Summary of the parent agent's TodoWrite task list, so sub-agents
    /// can distinguish done from pending work. Injected from the main
    /// agent's TaskStateStage.
    pub todo_summary: Option<String>,
    /// Top memory facts relevant to the parent task (T1 + relevant T2).
    /// Injected so sub-agents know what the main agent already knows.
    pub memory_context: Option<String>,
    /// Knowledge graph metadata (entity count etc.) for context awareness.
    pub kg_context: Option<String>,
}

/// Maximum recursion depth for sub-agent delegation.
pub const MAX_RECURSION_DEPTH: u32 = 3;

impl Default for SubAgentContext {
    fn default() -> Self {
        Self {
            system_info: String::new(),
            persona: "(not configured)".into(),
            skills: "(no skills registered)".into(),
            memory_facts: "(no relevant memory facts found)".into(),
            domain_knowledge: "(no domain knowledge matched)".into(),
            parent_workspace: None,
            delegation_note: None,
            permission_level: None,
            depth: 0,
            todo_summary: None,
            memory_context: None,
            kg_context: None,
        }
    }
}

impl SubAgentContext {
    /// Build the full sub-agent system prompt from all context sources.
    pub fn build_system_prompt(&self, task_description: &str) -> String {
        let mut prompt = String::new();

        // ── Locked system info ─────────────────────────────────
        if !self.system_info.is_empty() {
            prompt.push_str("## System Environment\n");
            prompt.push_str(&self.system_info);
            prompt.push_str("\n\n");
        }

        // ── Persona ────────────────────────────────────────────
        prompt.push_str("## Communication Style\n");
        prompt.push_str(&self.persona);
        prompt.push_str("\n\n");

        // ── Skills ─────────────────────────────────────────────
        prompt.push_str("## Available Skills\n");
        prompt.push_str(&self.skills);
        prompt.push_str("\n\n");

        // ── Memory ─────────────────────────────────────────────
        prompt.push_str("## Relevant Context\n");
        prompt.push_str(&self.memory_facts);
        prompt.push_str("\n\n");

        // ── Domain Knowledge ───────────────────────────────────
        prompt.push_str("## Domain Knowledge\n");
        prompt.push_str(&self.domain_knowledge);
        prompt.push_str("\n\n");

        // ── Parent workspace ───────────────────────────────────
        if let Some(ref pw) = self.parent_workspace {
            prompt.push_str("## Parent Workspace\n");
            prompt.push_str(&format!(
                "Files from the main agent are at: {}\n",
                pw.display()
            ));
            prompt.push_str("Use this path to access any files the main agent created.\n\n");
        }

        // ── Delegation note ────────────────────────────────────
        if let Some(ref note) = self.delegation_note {
            prompt.push_str("## Delegation Context\n");
            prompt.push_str(note);
            prompt.push_str("\n\n");
        }

        // ── Permission Level ───────────────────────────────────
        if let Some(ref level) = self.permission_level {
            prompt.push_str("## Permission Level\n");
            prompt.push_str(&format!("Parent session permission: {level}. "));
            if level == "全自动" || level == "fully_auto" {
                prompt.push_str(
                    "Your shell commands are auto-approved (except admin/sudo). \
                     Do NOT wait for or request confirmation — commands execute immediately.\n",
                );
            } else if level == "半自动" || level == "semi_auto" {
                prompt.push_str(
                    "Safe commands auto-run; dangerous ones require confirmation. \
                     If a command requires confirmation, it WILL be confirmed by the user \
                     transparently — just proceed.\n",
                );
            } else {
                prompt.push_str("Commands may require user confirmation before execution.\n");
            }
            prompt.push('\n');
        }

        // ── Delegation Depth ────────────────────────────────────
        if self.depth > 0 {
            prompt.push_str(&format!(
                "## Delegation Depth\n\
                 You are at depth {} (0 = main agent). ",
                self.depth,
            ));
            if self.depth < MAX_RECURSION_DEPTH {
                prompt.push_str(
                    "You may use the `task` tool to delegate sub-tasks. \
                     Sub-agents will be at depth {}.\n\n",
                );
                prompt = prompt.replace("{}", &(self.depth + 1).to_string());
            } else {
                prompt.push_str(
                    "You are at maximum delegation depth. \
                     Do NOT attempt to spawn further sub-agents.\n\n",
                );
            }
        }

        // ── Task State (inherited from parent agent) ───────────
        if let Some(ref ts) = self.todo_summary {
            if ts != "(empty)" {
                prompt.push_str("## Parent Agent Task State\n\n");
                prompt.push_str(ts);
                prompt.push_str("\n\n");
                prompt.push_str(
                    "Align your work with the parent's pending tasks. \
                     If the user says \"继续\" (continue), they mean resume the oldest \
                     PENDING task — not redo completed work.\n\n",
                );
            }
        }

        // ── Parent Memory Context (≤500 chars budget) ──────────
        if let Some(ref mc) = self.memory_context {
            let truncated: String = mc.chars().take(400).collect();
            prompt.push_str("## Parent Agent Memory\n\n");
            prompt.push_str(&truncated);
            prompt.push_str("\n\n");
        }
        if let Some(ref kg) = self.kg_context {
            prompt.push_str(&format!("Knowledge graph: {kg}\n\n"));
        }

        // ── Understanding User Intent ──────────────────────────
        prompt.push_str("## Understanding User Intent\n\n");
        prompt.push_str("- If the user reports they already did something: VERIFY, do NOT redo.\n");
        prompt.push_str(
            "- If the user says \"继续\" / \"continue\": resume the oldest \
             PENDING task, not the most recently discussed topic.\n",
        );
        prompt.push_str(
            "- Distinguish: \"I did X\" (verify) vs \"Do X\" (execute) vs \
             \"继续\" (resume pending).\n",
        );
        prompt.push_str("- Never repeat work the user states they completed.\n\n");

        // ── Rules ──────────────────────────────────────────────
        prompt.push_str("## Rules\n");
        prompt.push_str("- FAIL FAST: missing dependency → stop and report. No silent fallback.\n");
        prompt.push_str("- NO INSTALLS. Use what's available.\n");
        prompt.push_str("- Report ALL findings, including empty results.\n");
        prompt.push_str("- Return thorough, structured results.\n\n");

        // ── Task ───────────────────────────────────────────────
        prompt.push_str("## Task\n");
        prompt.push_str(task_description);

        prompt
    }
}

// ── Context assembler ──────────────────────────────────────────────────

/// Build a `SubAgentContext` by running every stage and capturing output.
/// Stages that return `None` get an explicit "(no ... found)" message
/// instead of being silently skipped.
#[allow(clippy::field_reassign_with_default, clippy::too_many_arguments)]
pub async fn assemble_subagent_context(
    user_message: &str,
    memory_stage: Option<&MemoryStage>,
    domain_stage: Option<&DomainKnowledgeStage>,
    parent_work_dir: Option<PathBuf>,
    delegation_note: Option<String>,
    shell_name: &str,
    tool_names: &[String],
    todo_summary: Option<String>,
    skill_list: Option<String>,
) -> SubAgentContext {
    let ctx = ContextBuildContext {
        user_message: user_message.to_string(),
        session_id: None,
        session_title: None,
        history: vec![],
        history_tokens: 0,
        max_context_tokens: 80000,
        shell_name: Some(shell_name.to_string()),
        permission_level: Some("semi_auto".into()),
        trusted_paths: vec![],
        tool_count: tool_names.len(),
        workspace_path: None,
        platform: None,
        git_branch: None,
        git_status: None,
        workspace_context_files: vec![],
        current_date: None,
        todo_summary: None,
        plan_mode: false,
        runtime_summary: None,
        sandbox_root: None,
        startup_verified: false,
        hook_feedback: None,
    };

    let mut sub_ctx = SubAgentContext::default();
    sub_ctx.system_info = build_system_info_block(shell_name, tool_names);

    // ── Skill list from registry ──────────────────────────────
    if let Some(ref skills) = skill_list {
        if !skills.is_empty() {
            sub_ctx.skills = skills.clone();
        }
    }

    // ── Memory ───────────────────────────────────────────────
    if let Some(stage) = memory_stage {
        match stage.build(&ctx) {
            Some(fragment) => {
                sub_ctx.memory_facts = fragment
                    .messages
                    .iter()
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            None => {
                sub_ctx.memory_facts = "(no relevant memory facts found for this task)".into();
            }
        }
    }

    // ── Domain knowledge ─────────────────────────────────────
    if let Some(stage) = domain_stage {
        match stage.build(&ctx) {
            Some(fragment) => {
                sub_ctx.domain_knowledge = fragment
                    .messages
                    .iter()
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            None => {
                sub_ctx.domain_knowledge = "(no domain knowledge matched for this task)".into();
            }
        }
    }

    // ── Parent workspace ─────────────────────────────────────
    sub_ctx.parent_workspace = parent_work_dir;

    // ── Delegation note ──────────────────────────────────────
    sub_ctx.delegation_note = delegation_note;

    // ── Todo state from parent agent ─────────────────────────
    sub_ctx.todo_summary = todo_summary;

    sub_ctx
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn build_system_info_block(shell_name: &str, tool_names: &[String]) -> String {
    let shell_guide = everevo_core::context::shell_specific_guide(shell_name);
    let tools_str = tool_names.join(", ");
    format!(
        "Shell: {shell}\n\
         Tools available: {tools}\n\
         Working directory: sandbox work dir (use ./ for paths)\n\
         Paths: use FORWARD slashes only\n\
         Encoding: UTF-8 enforced — all Python I/O defaults to UTF-8\n\
         \n\
         {guide}",
        shell = shell_name,
        tools = tools_str,
        guide = shell_guide,
    )
}
