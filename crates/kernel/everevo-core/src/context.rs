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

mod budget;
mod stages;

pub use budget::{
    estimate_tokens, ContextBudget, ContextBuildContext, ContextFragment, ContextSnapshot,
    StageSnapshot, CONTEXT_PREVIEW_MAX_CHARS, DEFAULT_CONTEXT_WINDOW,
};
pub use stages::{
    shell_specific_guide, ConversationHistoryStage, LatestMessageStage, RollingSummaryStage,
    SessionMetadataStage, SystemPromptStage, TaskStateStage,
};

use self::budget::truncate_content;
use crate::llm::LlmMessage;

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

    /// Whether this stage is exposed to the agent via the `pipeline` tool
    /// (tool-callable pipeline — "选择性复用管线部分"). Default false.
    fn tool_visible(&self) -> bool {
        false
    }

    /// One-line description for the `pipeline` tool's `list_stages`.
    fn description(&self) -> &str {
        ""
    }
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

                    // Auto-flag: oversized stage. When a per-model budget is set,
                    // flag when a stage exceeds its own cap; otherwise fall back
                    // to the legacy >40% of total-budget heuristic.
                    let budget_pct = (tokens as f64) / (max_budget as f64) * 100.0;
                    let stage_cap = if ctx.budget.window > 0 {
                        ctx.budget.stage(stage.name())
                    } else {
                        0
                    };
                    let status = if stage_cap > 0 && tokens > stage_cap {
                        flags.push(format!(
                            "Stage '{}' exceeds its budget (~{} tokens, got ~{tokens})",
                            stage.name(),
                            stage_cap
                        ));
                        "oversized"
                    } else if budget_pct > 40.0 {
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
            available_tokens: ctx.budget.available,
            safety_reserved: ctx.budget.safety_margin,
            output_reserved: ctx.budget.output_reserve,
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

// ── Builder Convenience ─────────────────────────────────────────────────

/// Build a default production pipeline with the standard stages.
///
/// Callers can add custom stages (RAG, KG, tools) via `with_stage()`.
pub fn default_pipeline() -> ContextPipeline {
    ContextPipeline::new()
        .with_stage(SystemPromptStage::new(SYSTEM_PROMPT))
        .with_stage(TaskStateStage)
        .with_stage(SessionMetadataStage)
        .with_stage(RollingSummaryStage)
        .with_stage(ConversationHistoryStage::default())
        .with_stage(LatestMessageStage)
}

/// Default system prompt — shell-specific instructions are injected
/// dynamically by SessionMetadataStage, so this stays shell-agnostic.
pub const SYSTEM_PROMPT: &str = "\
You are EverEvo, a desktop AI agent. Use tools to DO things — never just describe.\n\
\n\
## Tool Preferences\n\
\n\
You have specialized tools for common operations. Prefer them — they are safer,\n\
more reliable, and cheaper than shell. This is guidance, not a hard rule: use\n\
your judgment, and fall back to shell when a specialized tool fails or doesn't\n\
fit the task.\n\
\n\
| Operation | Prefer | Shell fallback |\n\
|-----------|---|---|\n\
| Read file | `read_file` | `shell cat` |\n\
| Write file | `write_file` | `shell echo` |\n\
| List dir | `list_dir` | `shell ls` |\n\
| Search code | `code_search` | `shell grep` |\n\
| Search web | `web_search` | `shell curl` |\n\
| Fetch URL | `web_fetch` | `shell curl` |\n\
| Download | `download` | `shell wget` |\n\
| Build/test/run | `shell` | — |\n\
| Git/packages | `shell` | — |\n\
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
- **Anti-fixation**: If the same command fails repeatedly, pause and diagnose the \
root cause (`which`, `echo $VAR`, read the error), web_search the error, and \
switch approach (SSH→HTTPS, different library). Retrying with minor tweaks \
rarely helps — when a loop forms, stop and reconsider instead.\
- **Vision / tool failure (image questions)**: if `describe_image` fails or \
times out twice, do NOT enter manual pixel/ASCII forensics — use the offline \
script (chess_fen.py / fractions_ocr.py) when one applies, otherwise commit a \
best-effort reading and mark it [UNVERIFIED]. A best-effort value beats an \
empty timeout.\n\
- **SSH→HTTPS**: Use `git clone https://...` and `gh` CLI. Never `git@github.com:`.\n\
- **Git auth**: Uses your global git config and SSH/HTTPS settings. \
  `gh` CLI uses stored OAuth. No extra credential setup needed.\n\
- **[SYSTEM NOTE] / [REQUIRED] messages**: Follow them — they're not suggestions.\n\
- **Type `/help`** to see slash commands. `/clear` resets context. `/compact` saves space.\n\
- **Admit when stuck**: \"I tried X, Y, Z. Here's what failed and what I need.\" \
Better than looping.\n\
- **Authoritative verification**: Time-sensitive or factual claims (dates, versions, \
APIs, current events, commands) — verify against authoritative web sources \
(`web_search`/`web_fetch`) before claiming done. Don't rely on memory or assumptions.";

// NOTE: the "我做了X = report not request" and "继续 = resume oldest PENDING"
// rules, and the verify-before-commit / never-weaken-tests / match-existing-
// style rules, live in ONE layer each (TaskStateStage / BestPracticesStage)
// to avoid the drift-bomb anti-pattern (one convention per layer).

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
