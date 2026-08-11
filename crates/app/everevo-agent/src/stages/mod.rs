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
//! [2] EvidenceChecklist — ECLoop-style pre-commit evidence checklist + deterministic verifier gate
//! [2] Skills            — available skill list (names + descriptions)
//! [3] Memory            — relevant memory facts (RRF-ranked)
//! [4] Domain Knowledge  — relevant domain document chunks
//! ```

pub mod agent_character;
pub mod answer_discipline;
pub mod best_practices;
pub mod domain_stage;
pub mod evidence_checklist;
pub mod memory;
pub mod persona;
pub mod pipeline;
pub mod skill;

pub use agent_character::{
    build_character_block, load_character, synthesize_character, AgentCharacter,
    AgentCharacterStage, SynthesisReport,
};
pub use answer_discipline::AnswerDisciplineStage;
pub use best_practices::BestPracticesStage;
pub use domain_stage::DomainKnowledgeStage;
pub use evidence_checklist::EvidenceChecklistStage;
pub use memory::MemoryStage;
pub use persona::{PersonaProfile, PersonaStage};
pub use pipeline::build_full_pipeline;
pub use skill::SkillStage;
