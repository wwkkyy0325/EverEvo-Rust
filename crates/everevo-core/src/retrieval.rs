//! Unified retrieval traits and fusion logic.
//!
//! ## Architecture (HiRA 2025 + RRF fusion)
//!
//! Decoupled retrieval: planners plan, retrievers retrieve, memory tracks state.
//! Each index (vector, FTS5, graph) implements a common trait.
//! The `HybridFusion` layer merges results via RRF or weighted score fusion.
//!
//! ## References
//! - HiRA (SIGIR 2026): decoupled planning/execution
//! - TopK 2025: RRF vs score fusion benchmarks (+3-8% nDCG with score fusion)
//! - RRF: k=60 default, weighted variant from Elasticsearch 2025

use serde::{Deserialize, Serialize};

// ── Search Result ─────────────────────────────────────────────────────────

/// A single search result from any retriever.
/// Scores are normalized to [0.0, 1.0] by the retriever implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Unique ID of the found item (chunk_id, fact_name, entity_id, etc.)
    pub id: String,
    /// Human-readable label or title.
    pub label: String,
    /// Content snippet (first 200 chars).
    pub snippet: String,
    /// Normalized relevance score [0.0, 1.0].
    pub score: f32,
    /// Which retriever produced this result (for debugging).
    pub source: String,
    /// Optional metadata (domain, session, entity type, etc.)
    pub metadata: serde_json::Value,
}

// ── Retrieval Router Trait ────────────────────────────────────────────────

/// A single retrieval backend (vector, FTS5, graph, etc.).
/// All retrievers implement this trait for pluggable hybrid fusion.
pub trait Retriever: Send + Sync {
    /// Unique name for logging and fusion weight assignment.
    fn name(&self) -> &str;

    /// Search and return top-k results with normalized scores [0.0, 1.0].
    fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult>;

    /// Whether this retriever is available (e.g., model loaded, index ready).
    fn available(&self) -> bool { true }
}

// ── Hybrid Fusion ─────────────────────────────────────────────────────────

/// Fuses results from multiple retrievers using Reciprocal Rank Fusion (RRF).
/// 2025 consensus: RRF with k=60 is the best zero-config starting point.
/// Weighted score fusion adds +3-8% nDCG when scores are properly normalized.
pub struct HybridFusion {
    /// RRF constant k (default: 60).
    pub rrf_k: f32,
    /// Per-retriever weights. If empty, all equal.
    pub weights: Vec<f32>,
}

impl Default for HybridFusion {
    fn default() -> Self {
        Self { rrf_k: 60.0, weights: Vec::new() }
    }
}

impl HybridFusion {
    pub fn new(rrf_k: f32) -> Self {
        Self { rrf_k, weights: Vec::new() }
    }

    /// Search across all retrievers and fuse results via RRF.
    /// Each retriever contributes `top_k` candidates; final output is ≤ `output_k`.
    pub fn search(
        &self,
        retrievers: &[Box<dyn Retriever>],
        query: &str,
        top_k: usize,
        output_k: usize,
    ) -> Vec<SearchResult> {
        // Phase 1: parallel retrieval from all backends
        let mut all_results: Vec<Vec<SearchResult>> = Vec::new();
        for r in retrievers {
            if r.available() {
                all_results.push(r.search(query, top_k));
            }
        }

        // Phase 2: RRF fusion
        self.fuse_rrf(&all_results, output_k)
    }

    /// Reciprocal Rank Fusion: score = Σ 1/(k + rank_i)
    /// Each result gets a score from every retriever list where it appears.
    fn fuse_rrf(&self, result_sets: &[Vec<SearchResult>], output_k: usize) -> Vec<SearchResult> {
        use std::collections::HashMap;

        // Map id → (best_result, accumulated_rrf_score)
        let mut fused: HashMap<String, (SearchResult, f32)> = HashMap::new();

        for (ri, results) in result_sets.iter().enumerate() {
            let weight = self.weights.get(ri).copied().unwrap_or(1.0);
            for (rank, result) in results.iter().enumerate() {
                let rrf_score = weight / (self.rrf_k + (rank + 1) as f32);
                fused
                    .entry(result.id.clone())
                    .and_modify(|(existing, score)| {
                        *score += rrf_score;
                        // Keep the result with higher individual score as representative
                        if result.score > existing.score {
                            *existing = result.clone();
                        }
                    })
                    .or_insert_with(|| (result.clone(), rrf_score));
            }
        }

        // Sort by RRF score descending
        let mut sorted: Vec<(SearchResult, f32)> = fused.into_values().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(output_k);

        sorted
            .into_iter()
            .map(|(mut result, rrf)| {
                result.score = rrf; // use RRF score as final
                result
            })
            .collect()
    }

    /// Weighted score fusion (z-score → sigmoid normalization).
    /// Use when retriever scores are meaningful and properly normalized.
    pub fn fuse_weighted(
        &self,
        result_sets: &[Vec<SearchResult>],
        output_k: usize,
    ) -> Vec<SearchResult> {
        use std::collections::HashMap;

        let mut fused: HashMap<String, (SearchResult, f32)> = HashMap::new();

        for (ri, results) in result_sets.iter().enumerate() {
            let weight = self.weights.get(ri).copied().unwrap_or(1.0);
            for result in results {
                let ws = result.score * weight;
                fused
                    .entry(result.id.clone())
                    .and_modify(|(existing, score)| {
                        *score += ws;
                        if result.score > existing.score {
                            *existing = result.clone();
                        }
                    })
                    .or_insert_with(|| (result.clone(), ws));
            }
        }

        let mut sorted: Vec<(SearchResult, f32)> = fused.into_values().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(output_k);
        sorted.into_iter().map(|(mut r, s)| { r.score = s; r }).collect()
    }
}

// ── Query Router (Agentic) ────────────────────────────────────────────────

/// Simple heuristic-based query classifier.
/// Phase 3b upgrades to LLM-based agentic routing.
#[derive(Debug, Clone)]
pub enum QueryType {
    /// Structured/metadata query → prefer graph
    Structured,
    /// Semantic/conceptual query → prefer vector
    Semantic,
    /// Mixed/hybrid → all sources
    Hybrid,
}

impl QueryType {
    pub fn classify(query: &str) -> Self {
        let lower = query.to_lowercase();
        let structured_keywords = ["多少", "几个", "count", "列出", "list", "哪些", "哪个"];
        let semantic_keywords = ["如何", "怎么", "为什么", "how", "why", "原理", "概念"];

        let has_structured = structured_keywords.iter().any(|k| lower.contains(k));
        let has_semantic = semantic_keywords.iter().any(|k| lower.contains(k));

        match (has_structured, has_semantic) {
            (true, false) => Self::Structured,
            (false, true) => Self::Semantic,
            _ => Self::Hybrid,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyRetriever {
        name: String,
        results: Vec<SearchResult>,
    }
    impl Retriever for DummyRetriever {
        fn name(&self) -> &str { &self.name }
        fn search(&self, _query: &str, _top_k: usize) -> Vec<SearchResult> {
            self.results.clone()
        }
    }

    fn make_result(id: &str, label: &str, score: f32) -> SearchResult {
        SearchResult { id: id.into(), label: label.into(), snippet: String::new(), score, source: "test".into(), metadata: serde_json::json!({}) }
    }

    #[test]
    fn test_rrf_fusion() {
        let r1 = Box::new(DummyRetriever { name: "a".into(), results: vec![
            make_result("1", "Doc1", 0.9),
            make_result("2", "Doc2", 0.7),
        ]});
        let r2 = Box::new(DummyRetriever { name: "b".into(), results: vec![
            make_result("2", "Doc2", 0.8),  // same doc, different score
            make_result("3", "Doc3", 0.6),
        ]});
        let retrievers: Vec<Box<dyn Retriever>> = vec![r1, r2];
        let fusion = HybridFusion::default();
        let results = fusion.search(&retrievers, "test", 10, 5);
        assert_eq!(results.len(), 3);
        // Doc2 should rank highest (appears in both lists)
        assert_eq!(results[0].id, "2");
    }

    #[test]
    fn test_query_classification() {
        assert!(matches!(QueryType::classify("有几个项目"), QueryType::Structured));
        assert!(matches!(QueryType::classify("如何优化性能"), QueryType::Semantic));
        assert!(matches!(QueryType::classify("有几个项目且如何优化"), QueryType::Hybrid));
    }

    #[test]
    fn test_weighted_fusion() {
        let r1 = Box::new(DummyRetriever { name: "vec".into(), results: vec![
            make_result("a", "A", 0.9),
        ]});
        let r2 = Box::new(DummyRetriever { name: "text".into(), results: vec![
            make_result("b", "B", 0.5),
        ]});
        let retrievers: Vec<Box<dyn Retriever>> = vec![r1, r2];
        let fusion = HybridFusion { rrf_k: 60.0, weights: vec![0.7, 0.3] };
        let results = fusion.fuse_weighted(&[retrievers[0].search("", 2), retrievers[1].search("", 2)], 5);
        assert_eq!(results.len(), 2);
        assert!(results[0].score > results[1].score);
    }
}
