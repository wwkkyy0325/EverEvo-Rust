//! Context pipeline stages — pluggable ContextStage implementations.
//!
//! Each stage runs during LLM context assembly with a priority order:
//!
//! ```text
//! [0] SystemPrompt     — static instructions + tool descriptions
//! [1] Persona          — user communication style + thinking paradigm
//! [2] BestPractices    — verification, planning, code quality rules
//! [2] Skills            — available skill list (names + descriptions)
//! [3] Memory            — relevant memory facts (RRF-ranked)
//! [4] Domain Knowledge  — relevant domain document chunks
//! ```

pub mod best_practices;
pub mod domain_stage;
pub mod memory;
pub mod persona;
pub mod skill;

pub use best_practices::BestPracticesStage;
pub use domain_stage::DomainKnowledgeStage;
pub use memory::MemoryStage;
pub use persona::{PersonaProfile, PersonaStage};
pub use skill::SkillStage;
