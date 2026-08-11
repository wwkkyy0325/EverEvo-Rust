//! Centralized context pipeline assembly.
//!
//! Every context stage is registered here in priority order.
//! To add a stage: impl `ContextStage`, then add one `.with_stage()` below.
//!
//! Priority order (lower = earlier in prompt):
//! ```text
//!   0: SystemPrompt     — static instructions + tool descriptions (from default_pipeline)
//!   0: AgentCharacter   — the agent's OWN voice / speaking style
//!   1: Persona          — user communication style + thinking paradigm
//!   2: BestPractices    — verification, planning, code quality rules
//!   2: AnswerDiscipline — final-answer marker, verbatim fidelity, constraint & candidate checks
//!   2: EvidenceChecklist — ECLoop-style pre-commit evidence checklist + deterministic verifier gate
//!   2: Skill            — loaded SKILL.md instructions
//!   3: TaskState        — current TodoWrite task list (from default_pipeline)
//!   3: Memory           — relevant memory facts (RRF-ranked)
//!   4: DomainKnowledge  — relevant domain document chunks
//!   5: SessionMetadata  — runtime env, shell, git status (from default_pipeline)
//!  80: ConversationHistory — current session messages, sliding window
//!  90: LatestMessage   — the new user input
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use everevo_core::context::{default_pipeline, ContextPipeline};

use super::{
    AgentCharacterStage, AnswerDisciplineStage, BestPracticesStage, DomainKnowledgeStage,
    EvidenceChecklistStage, MemoryStage, PersonaStage, SkillStage,
};
use crate::skill::SkillRegistry;

/// Build the complete production context pipeline with all 11 stages.
///
/// This replaces the manual `.with_stage()` chain previously in `chat.rs`
/// so all stage registration lives in one place — alongside the stage
/// implementations themselves.
///
/// The `memory_stage` and `domain_stage` are constructed by the caller
/// (they are request-scoped and depend on runtime state); everything
/// else is assembled here.
pub fn build_full_pipeline(
    agent_char_path: PathBuf,
    persona_profile_path: PathBuf,
    skill_registry: Arc<SkillRegistry>,
    memory_stage: MemoryStage,
    domain_stage: DomainKnowledgeStage,
) -> ContextPipeline {
    let mut pipeline = default_pipeline();

    // Priority 0 — agent's own voice (stable-sorts right after SystemPrompt)
    pipeline = pipeline.with_stage(AgentCharacterStage::new(agent_char_path));

    // Priority 1 — user communication style + thinking paradigm
    pipeline = pipeline.with_stage(PersonaStage::new(persona_profile_path));

    // Priority 2 — verification, planning, code quality rules
    pipeline = pipeline.with_stage(BestPracticesStage);

    // Priority 2 — answer-output discipline (final-answer marker, verbatim
    // fidelity, constraint enumeration, candidate verification). Stable-sorts
    // right after BestPractices, before skills.
    pipeline = pipeline.with_stage(AnswerDisciplineStage);

    // Priority 2 — verifier-gated commit (ECLoop-style evidence checklist:
    // pre-declare the constraints, verify each deterministically, then commit).
    // Stable-sorts right after AnswerDiscipline, before skills.
    pipeline = pipeline.with_stage(EvidenceChecklistStage);

    // Priority 2 — loaded SKILL.md instructions
    pipeline = pipeline.with_stage(SkillStage::new(skill_registry));

    // Priority 3 — relevant memory facts (RRF-ranked)
    pipeline = pipeline.with_stage(memory_stage);

    // Priority 4 — relevant domain document chunks
    pipeline = pipeline.with_stage(domain_stage);

    pipeline
}
