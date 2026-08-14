//! Context pipeline stages — pluggable ContextStage implementations.
//!
//! Each stage runs during LLM context assembly with a priority order:
//!
//! ```text
//! [0] SystemPrompt     — static instructions + tool descriptions
//! [0] AgentCharacter   — the agent's OWN voice / speaking style (after system prompt)
//! [1] Persona          — user communication style + thinking paradigm
//! [2] BestPractices    — verification, planning, code quality rules
//! [2] AnswerDiscipline — final-answer marker, verbatim fidelity, constraint & candidate checks
//! [2] Skills            — available skill list (names + descriptions)
//! [3] TaskState        — current TodoWrite task list
//! [3] Memory            — relevant memory facts (RRF-ranked)
//! [3] ProblemModeling  — causal-draft structural modeling (Hard only)
//! [3] VerifyCandidate  — independent skeptical verification
//! [3] EvidenceChecklist — ECLoop-style pre-commit checklist + deterministic gate
//! [4] Domain Knowledge  — relevant domain document chunks
//! [5] SessionMetadata  — runtime env, shell, git status
//! [75] RollingSummary  — rolling conversation summary
//! [80] ConversationHistory — session messages, sliding window
//! [90] LatestMessage   — the new user input
//! ```
//! (Authoritative order lives in pipeline.rs; keep this in sync.)

pub mod agent_character;
pub mod best_practices;
pub mod domain_stage;
pub mod memory;
pub mod persona;
pub mod pipeline;
pub mod skill;
pub mod verification;

pub(crate) use verification::clamp_content_by_tokens;

use everevo_core::context::ContextStage; // trait methods on stage types + `&dyn ContextStage` annotation

pub use agent_character::{
    build_character_block, load_character, synthesize_character, AgentCharacter,
    AgentCharacterStage, SynthesisReport,
};
pub use best_practices::BestPracticesStage;
pub use domain_stage::DomainKnowledgeStage;
pub use memory::MemoryStage;
pub use persona::{PersonaProfile, PersonaStage};
pub use pipeline::build_full_pipeline;
pub use skill::SkillStage;
// Verification ensemble — the four tool-visible verification stages + the
// difficulty gate. Grouped physically under stages/verification/ (2026-08-13).
pub use verification::{
    classify, hard_score, AnswerDisciplineStage, Difficulty, EvidenceChecklistStage,
    ProblemModelingStage, VerifyCandidateStage,
};

// ── Tool-callable pipeline catalog ───────────────────────────────────────

/// A stage exposed to the agent via the `pipeline` tool ("选择性复用管线部分").
#[derive(Debug, Clone)]
pub struct StageCatalogEntry {
    pub name: String,
    pub description: String,
    /// Short canonical guidance the agent applies when it invokes this stage
    /// on demand (the full fragment is auto-injected by default behavior).
    pub prompt: &'static str,
}

/// The static tool-visible stage instances — the SINGLE source the catalog
/// derives from (architecture-restructure-plan.md P1: "stage_catalog() 从
/// tool_visible() 元数据派生"). Adding a tool-visible stage = add it here and
/// to [`canonical_prompt`]; name/description are pulled from the instance, so
/// they can never drift from the stage itself.
const TOOL_VISIBLE_STAGES: [&dyn ContextStage; 4] = [
    &AnswerDisciplineStage,
    &EvidenceChecklistStage,
    &VerifyCandidateStage,
    &ProblemModelingStage,
];

/// The catalog of tool-visible stages (SELF-DISCOVER-style module library).
/// The `pipeline` tool lets the agent list these, run one, run a selected
/// subset, or compose a recommended sequence — while the DEFAULT pipeline still
/// auto-injects them by priority.
pub fn stage_catalog() -> Vec<StageCatalogEntry> {
    TOOL_VISIBLE_STAGES
        .iter()
        .map(|stage| StageCatalogEntry {
            name: stage.name().to_string(),
            description: stage.description().to_string(),
            prompt: canonical_prompt(stage.name()),
        })
        .collect()
}

/// Per-stage canonical guidance the agent applies when invoking the stage on
/// demand. Authored once here, keyed by stage name.
fn canonical_prompt(name: &str) -> &'static str {
    match name {
        "answer_discipline" => {
            "End your final message with a single `Final answer:` line containing ONLY \
             the value — bare number in the question's units, exact spelling, verbatim \
             list. Keep the epistemic boundary: commit only a [VERIFIED] value."
        }
        "evidence_checklist" => {
            "Enumerate every NUMBER, UNIT, NAMED ENTITY, and OPERATION the answer must \
             honor. Verify each deterministically (`verify_candidate.py`); on \
             disagreement escalate to `cluster verify`. Commit only when every item \
             has a source and a deterministic check."
        }
        "verify_candidate" => {
            "Act as an independent skeptical reviewer of the candidate: re-derive from \
             raw tool evidence. Check precision / magnitude / units / counts / \
             attribution / time. Commit only the value that survives every check."
        }
        "problem_modeling" => {
            "Build a structural problem model (`problem_model` tool): decompose into \
             sub-questions, tag each VERIFIED / UNVERIFIED / UNKNOWN, link \
             causal/evidence edges, research + deliberate, then answer each \
             sub-question with its [VERIFIED] source."
        }
        _ => "Apply this stage's guidance to the current task.",
    }
}

/// Recommend a stage sequence for a task description (SELF-DISCOVER-lite
/// `compose`): keyword-based, returns the canonical ordering.
pub fn compose_stages(task: &str) -> Vec<&'static str> {
    let lower = task.to_lowercase();
    let mut seq: Vec<&'static str> = Vec::new();
    // Complex / compound / multi-part → structural modeling first.
    if lower.contains("compound")
        || lower.contains("complex")
        || lower.contains("multi")
        || lower.contains("compare")
        || lower.contains("which of")
        || lower.contains("each")
    {
        seq.push("problem_modeling");
    }
    // Numeric / counting / units → the verification ensemble.
    if lower.chars().any(|c| c.is_ascii_digit())
        || [
            "count",
            "number of",
            "unit",
            "convert",
            "how many",
            "percent",
            "total",
        ]
        .iter()
        .any(|k| lower.contains(k))
    {
        seq.push("evidence_checklist");
        seq.push("verify_candidate");
    }
    // Always end with output discipline.
    seq.push("answer_discipline");
    seq
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog DERIVES from TOOL_VISIBLE_STAGES (P1 "完整派生"), so this
    /// guards the derivation contract instead of hand-checking a 4-entry list:
    /// every derived stage must actually be tool_visible, and the catalog must
    /// cover the source list 1:1 (no dupes, no drops, no drift in description).
    #[test]
    fn test_stage_catalog_derives_from_tool_visible_stages() {
        let catalog = stage_catalog();
        assert_eq!(
            catalog.len(),
            TOOL_VISIBLE_STAGES.len(),
            "catalog must cover every tool-visible stage exactly once"
        );
        for stage in &TOOL_VISIBLE_STAGES {
            assert!(
                stage.tool_visible(),
                "stage {} is cataloged but NOT tool_visible",
                stage.name()
            );
            let entry = catalog
                .iter()
                .find(|e| e.name == stage.name())
                .unwrap_or_else(|| panic!("catalog missing tool-visible stage {}", stage.name()));
            assert_eq!(
                entry.description,
                stage.description(),
                "catalog description for {} drifted from the stage",
                stage.name()
            );
        }
        // Every cataloged entry is one of the derived stages (1:1 by name).
        let names: Vec<String> = catalog.iter().map(|e| e.name.clone()).collect();
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "catalog contains duplicate stage names"
        );
    }

    #[test]
    fn test_compose_stages_always_ends_with_answer_discipline() {
        let seq = compose_stages("compound multi-part numeric problem");
        assert_eq!(seq.last(), Some(&"answer_discipline"));
    }
}
