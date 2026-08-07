//! Memory Consolidator — semantic deduplication, scoring, and automatic
//! consolidation of long-term memory facts.
//!
//! ## References
//! - Mem0 (arXiv:2504.19413): ADD/UPDATE/DELETE/NOOP consolidation model
//! - OpenClaw Dreaming: 6-dimension scoring with threshold gating

use everevo_core::memory::MemoryFact;

/// Result of comparing a candidate fact against existing memories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsolidationAction {
    /// Insert as a new fact.
    Add,
    /// Update an existing fact (semantically equivalent, new info).
    Update {
        existing_name: String,
        reason: String,
    },
    /// Delete an existing fact (contradicted or obsolete).
    Delete {
        existing_name: String,
        reason: String,
    },
    /// No operation — fact is already present or irrelevant.
    Noop { reason: String },
}

/// A scored candidate from the DEEP phase.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub fact: MemoryFact,
    /// 6-dimension composite score [0.0, 1.0].
    pub score: f32,
    /// Individual dimension scores for debugging.
    pub dimension_scores: DimensionScores,
    /// Recommended action after comparing against existing facts.
    pub action: ConsolidationAction,
}

#[derive(Debug, Clone, Default)]
pub struct DimensionScores {
    pub relevance: f32,
    pub frequency: f32,
    pub query_diversity: f32,
    pub recency: f32,
    pub consolidation: f32,
    pub conceptual_richness: f32,
}

/// Default scoring weights (OpenClaw model, validated in production).
const WEIGHTS: [f32; 6] = [0.30, 0.24, 0.15, 0.15, 0.10, 0.06];

/// Thresholds for DEEP phase gating.
const MIN_SCORE: f32 = 0.45;
const MIN_RECALL_COUNT: u32 = 2;
const MIN_UNIQUE_QUERIES: u32 = 1;

/// Memory Consolidator.
pub struct MemoryConsolidator {
    /// Semantic similarity threshold for considering two facts "about the same thing".
    pub similarity_threshold: f32,
}

impl Default for MemoryConsolidator {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.85,
        }
    }
}

impl MemoryConsolidator {
    pub fn new(similarity_threshold: f32) -> Self {
        Self {
            similarity_threshold,
        }
    }

    /// Score a candidate fact on 6 dimensions.
    ///
    /// `recall_count` — how many times this fact (or similar) was recalled
    /// `unique_days`    — how many distinct days this fact appeared
    /// `concept_tags`   — number of distinct concept tags in the fact
    pub fn score(
        &self,
        fact: &MemoryFact,
        recall_count: u32,
        unique_days: u32,
        concept_tags: usize,
    ) -> ScoredCandidate {
        let dims = DimensionScores {
            relevance: fact.projection.confidence,
            frequency: (recall_count as f32 / 10.0).min(1.0),
            query_diversity: (unique_days as f32 / 7.0).min(1.0),
            recency: recency_decay(fact.created_at),
            consolidation: if recall_count >= 3 { 0.8 } else { 0.3 },
            conceptual_richness: (concept_tags as f32 / 5.0).min(1.0),
        };

        let composite = dims.relevance * WEIGHTS[0]
            + dims.frequency * WEIGHTS[1]
            + dims.query_diversity * WEIGHTS[2]
            + dims.recency * WEIGHTS[3]
            + dims.consolidation * WEIGHTS[4]
            + dims.conceptual_richness * WEIGHTS[5];

        ScoredCandidate {
            fact: fact.clone(),
            score: composite,
            dimension_scores: dims,
            action: ConsolidationAction::Noop {
                reason: "Not yet compared against existing facts".into(),
            },
        }
    }

    /// Determine the consolidation action for a candidate by comparing
    /// against existing facts using semantic similarity.
    ///
    /// `similarity_fn` — computes cosine similarity between two fact vectors.
    pub fn consolidate(
        &self,
        candidate: &MemoryFact,
        existing: &[MemoryFact],
    ) -> ConsolidationAction {
        // Simple keyword overlap as a proxy for semantic similarity
        // (full vector comparison happens in the vector store)
        let cand_words: Vec<&str> = candidate
            .content
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|w| w.len() > 2)
            .collect();

        let mut best_match: Option<(&MemoryFact, f32)> = None;

        for ex in existing {
            if ex.name == candidate.name {
                return ConsolidationAction::Update {
                    existing_name: ex.name.clone(),
                    reason: "Same fact name — updating content".into(),
                };
            }

            let ex_words: Vec<&str> = ex
                .content
                .split_whitespace()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                .filter(|w| w.len() > 2)
                .collect();

            let overlap = jaccard_similarity(&cand_words, &ex_words);

            if overlap > self.similarity_threshold {
                if let Some((_, prev_score)) = best_match {
                    if overlap > prev_score {
                        best_match = Some((ex, overlap));
                    }
                } else {
                    best_match = Some((ex, overlap));
                }
            }
        }

        match best_match {
            Some((ex, score)) if score > 0.95 => ConsolidationAction::Noop {
                reason: format!(
                    "Nearly identical to existing fact '{}' (sim={:.3})",
                    ex.name, score
                ),
            },
            Some((ex, score)) => ConsolidationAction::Update {
                existing_name: ex.name.clone(),
                reason: format!("Similar to existing fact '{}' (sim={:.3})", ex.name, score),
            },
            None => ConsolidationAction::Add,
        }
    }

    /// Apply threshold gating — does this candidate pass the DEEP phase gates?
    pub fn passes_gates(candidate: &ScoredCandidate, recall_count: u32, unique_days: u32) -> bool {
        candidate.score >= MIN_SCORE
            && recall_count >= MIN_RECALL_COUNT
            && unique_days >= MIN_UNIQUE_QUERIES
    }

    /// Find facts that may need deprecation (low relevance, old, never retrieved).
    pub fn find_stale_candidates(facts: &[MemoryFact], max_count: usize) -> Vec<&MemoryFact> {
        let now = chrono::Utc::now();
        let threshold = chrono::Duration::days(90);

        facts
            .iter()
            .filter(|f| {
                let age = now - f.updated_at;
                age > threshold && f.projection.confidence < 0.5
            })
            .take(max_count)
            .collect()
    }
}

/// Compute Jaccard similarity between two word sets.
fn jaccard_similarity(a: &[&str], b: &[&str]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection: usize = a.iter().filter(|w| b.contains(w)).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// Exponential recency decay: newer = higher score.
fn recency_decay(created_at: chrono::DateTime<chrono::Utc>) -> f32 {
    let age_hours = (chrono::Utc::now() - created_at).num_hours() as f32;
    // Half-life: 7 days (168 hours)
    let half_life: f32 = 168.0;
    0.5_f32.powf(age_hours / half_life)
}

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::{FactType, ProjectionMetadata};

    fn make_fact(name: &str, content: &str) -> MemoryFact {
        MemoryFact {
            name: name.into(),
            description: "test".into(),
            content: content.into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 0.8),
            links: vec![],
        }
    }

    #[test]
    fn test_identical_fact_is_update() {
        let c = MemoryConsolidator::default();
        let cand = make_fact(
            "pref",
            "User prefers async await over promise chains in all projects",
        );
        let existing = vec![make_fact(
            "pref",
            "User prefers async await over promise chains in all projects",
        )];
        let action = c.consolidate(&cand, &existing);
        assert!(matches!(action, ConsolidationAction::Update { .. }));
    }

    #[test]
    fn test_similar_fact_is_update() {
        let mut c = MemoryConsolidator::default();
        c.similarity_threshold = 0.4; // lower threshold for test
        let cand = make_fact("pref-v2", "User prefers async await for JavaScript code");
        let existing = vec![make_fact(
            "pref",
            "User prefers async await for all code including JavaScript",
        )];
        let action = c.consolidate(&cand, &existing);
        assert!(
            matches!(action, ConsolidationAction::Update { .. }),
            "Expected Update, got {:?}",
            action
        );
    }

    #[test]
    fn test_unrelated_fact_is_add() {
        let c = MemoryConsolidator::default();
        let cand = make_fact("python-version", "Project uses Python 3.11");
        let existing = vec![make_fact(
            "pref",
            "User prefers async/await over promise chains",
        )];
        let action = c.consolidate(&cand, &existing);
        assert_eq!(action, ConsolidationAction::Add);
    }

    #[test]
    fn test_gating_rejects_low_score() {
        let cand = ScoredCandidate {
            fact: make_fact("x", "y"),
            score: 0.3,
            dimension_scores: DimensionScores::default(),
            action: ConsolidationAction::Add,
        };
        assert!(!MemoryConsolidator::passes_gates(&cand, 1, 1));
    }

    #[test]
    fn test_jaccard() {
        let a: Vec<&str> = "a b c".split_whitespace().collect();
        let b: Vec<&str> = "b c d".split_whitespace().collect();
        let sim = jaccard_similarity(&a, &b);
        assert!((sim - 0.5).abs() < 0.01);
    }
}
