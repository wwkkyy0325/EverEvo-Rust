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

use crate::memory::MemoryStage;
use crate::DomainKnowledgeStage;

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
}

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
pub async fn assemble_subagent_context(
    user_message: &str,
    memory_stage: Option<&MemoryStage>,
    domain_stage: Option<&DomainKnowledgeStage>,
    parent_work_dir: Option<PathBuf>,
    delegation_note: Option<String>,
    shell_name: &str,
    tool_names: &[String],
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
    };

    let mut sub_ctx = SubAgentContext::default();

    // ── System info (always present) ─────────────────────────
    sub_ctx.system_info = build_system_info_block(shell_name, tool_names);

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
                sub_ctx.memory_facts =
                    "(no relevant memory facts found for this task)".into();
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
                sub_ctx.domain_knowledge =
                    "(no domain knowledge matched for this task)".into();
            }
        }
    }

    // ── Parent workspace ─────────────────────────────────────
    sub_ctx.parent_workspace = parent_work_dir;

    // ── Delegation note ──────────────────────────────────────
    sub_ctx.delegation_note = delegation_note;

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
