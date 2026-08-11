//! Information Retrieval metrics — NDCG, Recall, MRR, Precision.
//!
//! Implements the standard evaluation metrics used by BEIR (NeurIPS 2021),
//! TREC, and RAGAS frameworks. All metrics operate on ranked result lists
//! and relevance judgments.
//!
//! ## References
//! - BEIR: <https://github.com/beir-cellar/beir> (NDCG@10 primary metric)
//! - TREC eval: standard IR metric definitions
//! - RAGAS: Context Precision / Context Recall

use std::collections::HashMap;

/// Compute Normalized Discounted Cumulative Gain at rank k.
///
/// NDCG@k is the primary BEIR metric. It measures ranking quality by
/// accounting for both relevance grades and position (discount).
///
/// Formula: NDCG@k = DCG@k / IDCG@k
/// where DCG@k = Σ(i=1..k) (2^rel_i - 1) / log2(i + 1)
///
/// `results`: ordered list of doc IDs (best first)
/// `qrels`: doc_id → relevance_score (typically 0-3 in BEIR)
/// `k`: cutoff rank
pub fn ndcg_at_k(results: &[String], qrels: &HashMap<String, u32>, k: usize) -> f64 {
    let k = k.min(results.len());
    if k == 0 {
        return 0.0;
    }

    // DCG@k
    let dcg: f64 = results
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, doc_id)| {
            let rel = qrels.get(doc_id).copied().unwrap_or(0) as f64;
            let gain = 2.0_f64.powf(rel) - 1.0;
            let discount = 1.0 / ((i + 2) as f64).log2();
            gain * discount
        })
        .sum();

    // IDCG@k — ideal ranking (sorted by relevance desc)
    let mut ideal_rel: Vec<u32> = qrels.values().copied().collect();
    ideal_rel.sort_by(|a, b| b.cmp(a));
    let idcg: f64 = ideal_rel
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &rel)| {
            let gain = 2.0_f64.powf(rel as f64) - 1.0;
            let discount = 1.0 / ((i + 2) as f64).log2();
            gain * discount
        })
        .sum();

    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// Compute Recall at rank k.
///
/// Recall@k = |relevant ∩ top_k| / |all_relevant|
///
/// This is the fraction of all relevant documents that were retrieved
/// in the top-k results. It measures retrieval completeness.
pub fn recall_at_k(results: &[String], qrels: &HashMap<String, u32>, k: usize) -> f64 {
    let k = k.min(results.len());
    let total_relevant = qrels.values().filter(|&&v| v > 0).count();
    if total_relevant == 0 {
        return 0.0;
    }

    let retrieved_relevant = results
        .iter()
        .take(k)
        .filter(|doc_id| qrels.get(doc_id.as_str()).copied().unwrap_or(0) > 0)
        .count();

    retrieved_relevant as f64 / total_relevant as f64
}

/// Compute Mean Reciprocal Rank.
///
/// MRR = (1 / |Q|) * Σ(1 / rank_of_first_relevant)
///
/// Measures how quickly (on average) the first relevant document appears.
/// Range: [0, 1]. Higher is better. 1.0 means the first result was always relevant.
pub fn reciprocal_rank(results: &[String], qrels: &HashMap<String, u32>) -> f64 {
    for (i, doc_id) in results.iter().enumerate() {
        if qrels.get(doc_id.as_str()).copied().unwrap_or(0) > 0 {
            return 1.0 / ((i + 1) as f64);
        }
    }
    0.0
}

pub fn mean_reciprocal_rank(
    all_results: &[Vec<String>],
    all_qrels: &[HashMap<String, u32>],
) -> f64 {
    let n = all_results.len().min(all_qrels.len());
    if n == 0 {
        return 0.0;
    }
    all_results[..n]
        .iter()
        .zip(all_qrels[..n].iter())
        .map(|(results, qrels)| reciprocal_rank(results, qrels))
        .sum::<f64>()
        / n as f64
}

/// Compute Precision at rank k.
///
/// Precision@k = |relevant ∩ top_k| / k
pub fn precision_at_k(results: &[String], qrels: &HashMap<String, u32>, k: usize) -> f64 {
    let k = k.min(results.len());
    if k == 0 {
        return 0.0;
    }

    let hits = results
        .iter()
        .take(k)
        .filter(|doc_id| qrels.get(doc_id.as_str()).copied().unwrap_or(0) > 0)
        .count();

    hits as f64 / k as f64
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_qrels(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn test_ndcg_perfect_ranking() {
        let qrels = make_qrels(&[("a", 3), ("b", 2), ("c", 1)]);
        let results: Vec<String> = vec!["a", "b", "c"].into_iter().map(String::from).collect();
        let score = ndcg_at_k(&results, &qrels, 3);
        assert!(
            (score - 1.0).abs() < 1e-9,
            "Perfect ranking NDCG@3 should be 1.0, got {score}"
        );
    }

    #[test]
    fn test_ndcg_worst_ranking() {
        let qrels = make_qrels(&[("a", 3), ("b", 2), ("c", 1)]);
        // Worst: least relevant first
        let results: Vec<String> = vec!["c", "b", "a"].into_iter().map(String::from).collect();
        let score = ndcg_at_k(&results, &qrels, 3);
        assert!(
            score < 1.0,
            "Worst ranking NDCG should be < 1.0, got {score}"
        );
        assert!(
            score > 0.0,
            "Worst ranking NDCG should be > 0.0, got {score}"
        );
    }

    #[test]
    fn test_ndcg_no_relevant() {
        let qrels: HashMap<String, u32> = HashMap::new();
        let results: Vec<String> = vec!["x", "y", "z"].into_iter().map(String::from).collect();
        assert_eq!(ndcg_at_k(&results, &qrels, 3), 0.0);
    }

    #[test]
    fn test_recall_perfect() {
        let qrels = make_qrels(&[("a", 1), ("b", 1)]);
        let results: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        assert!((recall_at_k(&results, &qrels, 2) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_recall_partial() {
        let qrels = make_qrels(&[("a", 1), ("b", 1), ("c", 1), ("d", 1)]);
        let results: Vec<String> = vec!["a", "c", "x", "y"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!((recall_at_k(&results, &qrels, 4) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_mrr_first_position() {
        let qrels = make_qrels(&[("target", 1)]);
        let results: Vec<String> = vec!["target", "b", "c"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!((reciprocal_rank(&results, &qrels) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_mrr_second_position() {
        let qrels = make_qrels(&[("target", 1)]);
        let results: Vec<String> = vec!["a", "target", "c"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!((reciprocal_rank(&results, &qrels) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_mrr_not_found() {
        let qrels = make_qrels(&[("target", 1)]);
        let results: Vec<String> = vec!["a", "b", "c"].into_iter().map(String::from).collect();
        assert_eq!(reciprocal_rank(&results, &qrels), 0.0);
    }

    #[test]
    fn test_precision_perfect() {
        let qrels = make_qrels(&[("a", 1), ("b", 1)]);
        let results: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        assert!((precision_at_k(&results, &qrels, 2) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_precision_half() {
        let qrels = make_qrels(&[("a", 1)]);
        let results: Vec<String> = vec!["a", "x"].into_iter().map(String::from).collect();
        assert!((precision_at_k(&results, &qrels, 2) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_ndcg_empty_results() {
        let qrels = make_qrels(&[("a", 1)]);
        let results: Vec<String> = vec![];
        assert_eq!(ndcg_at_k(&results, &qrels, 10), 0.0);
        assert_eq!(recall_at_k(&results, &qrels, 10), 0.0);
        assert_eq!(precision_at_k(&results, &qrels, 10), 0.0);
    }
}
