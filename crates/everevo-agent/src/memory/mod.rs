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
pub mod facts;
pub mod frontmatter;
pub mod index;
pub mod pipeline;
pub mod scheduler;
pub mod wiki;

use std::sync::Arc;
use std::time::Instant;

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;
use everevo_core::memory::MemoryIndexEntry;
use everevo_telemetry::{RetrievalRecord, Telemetry};
use uuid::Uuid;

use self::facts::FactManager;

// Re-export main types
pub use self::consolidator::{ConsolidationAction, DimensionScores, MemoryConsolidator, ScoredCandidate};
pub use self::diary::{DiaryEntry, DiaryManager};
pub use self::engine::DreamingEngine;
pub use self::facts::FactManager as FactStore;
pub use self::frontmatter::{parse_fact_file, parse_frontmatter, serialize_fact_file};
pub use self::index::{load_all_facts, parse_index, regenerate_index};
pub use self::pipeline::{ApplyStats, ChunkExtractor, ExtractionResult, ExtractedEntity, ExtractedRelation};
pub use self::scheduler::{DreamingScheduler, SchedulerConfig, ScheduledPhase};
pub use self::wiki::WikiGenerator;

// ── MemoryStage (ContextPipeline Integration) ─────────────────────────────

/// Injects relevant memory facts into the LLM context pipeline.
///
/// Instead of blindly injecting the first N lines of MEMORY.md,
/// this stage performs a **keyword-based relevance match** against
/// the current user message and injects only the most relevant facts.
/// Falls back to the MEMORY.md index lean if no match is found.
pub struct MemoryStage {
    fact_manager: Arc<FactManager>,
    /// Maximum number of matched facts to inject.
    max_facts: usize,
    /// Maximum token budget for injected memory (~500 tokens).
    #[allow(dead_code)]
    max_tokens: usize,
    /// Optional telemetry handle for recording retrieval metrics.
    telemetry: Option<Arc<Telemetry>>,
    /// Trace ID for correlating telemetry records.
    trace_id: Option<Uuid>,
}

impl MemoryStage {
    pub fn new(fact_manager: Arc<FactManager>) -> Self {
        Self {
            fact_manager,
            max_facts: 5,
            max_tokens: 500,
            telemetry: None,
            trace_id: None,
        }
    }

    /// Attach telemetry for recording retrieval metrics.
    pub fn with_telemetry(mut self, telemetry: Arc<Telemetry>, trace_id: Uuid) -> Self {
        self.telemetry = Some(telemetry);
        self.trace_id = Some(trace_id);
        self
    }

    pub fn with_max_facts(mut self, n: usize) -> Self {
        self.max_facts = n;
        self
    }

    /// Find facts relevant to the user's current message.
    /// Uses RRF fusion of keyword + content overlap for robust ranking.
    /// Phase 3 upgrades to FTS5+vector hybrid via everevo-core::retrieval::HybridFusion.
    fn find_relevant(&self, user_message: &str) -> Vec<MemoryIndexEntry> {
        let start = Instant::now();

        let Ok(all_facts) = self.fact_manager.load_all() else {
            return vec![];
        };
        if all_facts.is_empty() || user_message.trim().is_empty() {
            return vec![];
        }

        // Two-stage: keyword rank + content overlap rank → RRF fusion
        let query_lower = user_message.to_lowercase();
        let query_words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        let mut keyword_scores: Vec<(usize, f32)> = Vec::new();  // (fact_index, rank)
        let mut content_scores: Vec<(usize, f32)> = Vec::new();

        for (i, fact) in all_facts.iter().enumerate() {
            let fact_text = format!("{} {} {}", fact.name, fact.description, fact.content).to_lowercase();

            // Keyword score: how many query words appear in the fact
            let kw_matches = query_words.iter().filter(|w| fact_text.contains(*w)).count();
            if kw_matches > 0 {
                keyword_scores.push((i, kw_matches as f32 / query_words.len().max(1) as f32));
            }

            // Content overlap: Jaccard on significant words
            let fact_words: std::collections::HashSet<&str> = fact_text
                .split_whitespace().filter(|w| w.len() > 2).collect();
            let query_set: std::collections::HashSet<&str> = query_words.iter().copied().collect();
            let intersection = fact_words.intersection(&query_set).count();
            let union = fact_words.len() + query_set.len() - intersection;
            if union > 0 {
                let jaccard = intersection as f32 / union as f32;
                if jaccard > 0.0 {
                    content_scores.push((i, jaccard));
                }
            }
        }

        // RRF-like fusion: rank within each list, then merge
        keyword_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        content_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let k: f32 = 60.0;
        let mut fused: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
        for (rank, (i, _)) in keyword_scores.iter().enumerate() {
            *fused.entry(*i).or_default() += 1.0 / (k + (rank + 1) as f32);
        }
        for (rank, (i, _)) in content_scores.iter().enumerate() {
            *fused.entry(*i).or_default() += 1.0 / (k + (rank + 1) as f32);
        }

        let mut ranked: Vec<(usize, f32)> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(self.max_facts);

        let results: Vec<MemoryIndexEntry> = ranked
            .into_iter()
            .filter_map(|(i, _)| all_facts.get(i))
            .map(|fact| MemoryIndexEntry {
                name: fact.name.clone(),
                description: fact.description.clone(),
                fact_type: fact.fact_type.clone(),
            })
            .collect();

        // Record retrieval telemetry (fire-and-forget)
        if let (Some(telemetry), Some(trace_id)) = (&self.telemetry, self.trace_id) {
            let latency_ms = start.elapsed().as_millis() as i64;
            telemetry.record_retrieval(RetrievalRecord {
                trace_id,
                query: user_message.to_string(),
                source: "memory-rrf".into(),
                recall_k: results.len() as i32,
                precision_at_5: None,
                mrr: None,
                latency_ms,
                experiment_id: None,
                variant: None,
            });
        }

        results
    }
}

impl ContextStage for MemoryStage {
    fn priority(&self) -> i32 {
        3 // after system prompt (0), before session metadata (5)
    }

    fn name(&self) -> &str {
        "memory"
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let relevant = self.find_relevant(&ctx.user_message);

        if relevant.is_empty() {
            // No relevant facts — load index lean as fallback
            let index = self
                .fact_manager
                .read_index_lean(50)
                .ok()?;
            if index.is_empty() {
                return None;
            }
            let content = format!(
                "## Persistent Memory\n\
                 The following facts have been saved from previous sessions:\n\n\
                 {index}"
            );
            return Some(ContextFragment {
                label: "Memory Index".into(),
                messages: vec![LlmMessage::user(&content)],
            });
        }

        let lines: Vec<String> = relevant
            .iter()
            .map(|e| format!("- [{name}](facts/{name}.md) \u{2014} {desc}",
                name = e.name, desc = e.description))
            .collect();

        let content = format!(
            "## Relevant Memory\n\
             The following saved facts are relevant to the current conversation:\n\n\
             {}\n\n\
             Use the `memory` tool with `search` to find more if needed.",
            lines.join("\n")
        );

        Some(ContextFragment {
            label: "Relevant Memory".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}

// WikiGenerator re-exported from wiki.rs
