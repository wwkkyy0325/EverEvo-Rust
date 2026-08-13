//! Verification ensemble — the deterministic verifier + evidence checklist +
//! output discipline + problem modeling for HARD questions.
//!
//! Physical grouping of the verification subsystem (2026-08-13 restructure):
//! - `gate.rs` — difficulty classifier + context-clamp helpers
//! - `skeptic.rs` — EvidenceChecklist + VerifyCandidate stages
//! - `discipline.rs` — AnswerDiscipline stage
//! - `modeling.rs` — ProblemModeling stage
//!
//! The parent `stages::` module re-exports these so external paths are
//! unchanged.

pub mod discipline;
pub mod gate;
pub mod modeling;
pub mod skeptic;

pub use discipline::AnswerDisciplineStage;
pub(crate) use gate::clamp_content_by_tokens;
pub use gate::{classify, hard_score, Difficulty};
pub use modeling::ProblemModelingStage;
pub use skeptic::{EvidenceChecklistStage, VerifyCandidateStage};
