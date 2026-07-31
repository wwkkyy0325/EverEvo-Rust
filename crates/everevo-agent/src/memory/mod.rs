//! EverEvo Memory System — persistent context across sessions.
//!
//! ## Architecture
//!
//! ```text
//! data/memory/
//!   ├── diary/          ← LIGHT phase: raw SQLite → LLM-trimmed daily notes
//!   ├── facts/          ← long-term facts (Agent-managed, human-editable)
//!   ├── MEMORY.md       ← auto-generated index from facts/
//!   ├── .dreams/        ← pipeline internal state (themes, signals, candidates)
//!   ├── vector/         ← LanceDB chunks (Phase 2b)
//!   └── graph/          ← Oxigraph knowledge graph (Phase 2b)
//! ```
//!
//! ## Data Flow
//!
//! ```text
//! SQLite (immutable) → Diary (LIGHT) → Themes (REM) → Facts (DEEP) → Wiki
//! User "remember X"  → Facts (实时写入)
//! ```

pub mod consolidator;
pub mod diary;
pub mod engine;
pub mod extractor;
pub mod facts;
pub mod frontmatter;
pub mod index;
pub mod pipeline;
pub mod reflection;
pub mod scheduler;
pub mod wiki;

// Re-export main types
pub use self::consolidator::{
    ConsolidationAction, DimensionScores, MemoryConsolidator, ScoredCandidate,
};
pub use self::diary::{DiaryEntry, DiaryManager};
pub use self::engine::DreamingEngine;
pub use self::facts::FactManager as FactStore;
pub use self::frontmatter::{parse_fact_file, parse_frontmatter, serialize_fact_file};
pub use self::index::{load_all_facts, parse_index, regenerate_index};
pub use self::pipeline::{
    ApplyStats, ChunkExtractor, ExtractedEntity, ExtractedRelation, ExtractionResult,
};
pub use self::scheduler::{DreamingScheduler, ScheduledPhase, SchedulerConfig};
pub use self::wiki::WikiGenerator;

// MemoryStage moved to crate::stages::memory

// WikiGenerator re-exported from wiki.rs
