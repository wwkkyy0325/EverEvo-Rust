//! Entity resolver — 3-phase deduplication: Blocking → Matching → Merging.
//!
//! ## References
//! - DEG-RAG (arXiv:2510.14271): type-aware blocking, KG embedding matching
//! - Agentic-KGR (arXiv:2510.09156): 98.5% deduplication accuracy

use std::collections::HashMap;

use super::graph::KnowledgeGraph;
use super::types::Entity;

/// Result of comparing two entities for potential merging.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// Composite score [0.0, 1.0], higher = more likely match.
    pub score: f32,
    /// Whether this pair should be merged.
    pub is_match: bool,
    /// Human-readable reason for the decision.
    pub reason: String,
}

/// Statistics from an entity resolution pass.
#[derive(Debug, Clone, Default)]
pub struct ResolveStats {
    /// Number of candidate pairs checked.
    pub pairs_checked: usize,
    /// Number of matches found above threshold.
    pub matches_found: usize,
    /// Number of entities actually merged.
    pub entities_merged: usize,
}

/// 3-phase entity resolver: Blocking -> Matching -> Merging.
pub struct EntityResolver {
    /// Score threshold for considering two entities a match.
    pub match_threshold: f32,
}

impl Default for EntityResolver {
    fn default() -> Self {
        Self {
            match_threshold: 0.7,
        }
    }
}

impl EntityResolver {
    pub fn new(match_threshold: f32) -> Self {
        Self { match_threshold }
    }

    // ── Phase 1: Blocking ───────────────────────────────────────────────

    /// Find candidate pairs that MIGHT be duplicates.
    pub fn find_candidate_pairs<'a>(
        &self,
        entities: &'a [Entity],
    ) -> Vec<(&'a Entity, &'a Entity)> {
        let mut pairs = Vec::new();

        let mut by_type: HashMap<String, Vec<&Entity>> = HashMap::new();
        for e in entities {
            if e.merged_into.is_some() {
                continue;
            }
            by_type
                .entry(e.entity_type.to_string())
                .or_default()
                .push(e);
        }

        for group in by_type.values() {
            for i in 0..group.len() {
                for j in (i + 1)..group.len() {
                    let a = group[i];
                    let b = group[j];
                    let a_prefix = normalize_prefix(&a.label, 3);
                    let b_prefix = normalize_prefix(&b.label, 3);
                    if a_prefix == b_prefix || a_prefix.is_empty() || b_prefix.is_empty() {
                        pairs.push((a, b));
                    }
                }
            }
        }

        pairs
    }

    // ── Phase 2: Matching ───────────────────────────────────────────────

    /// Compare two entities and return a match result.
    pub fn match_pair(&self, a: &Entity, b: &Entity) -> Option<MatchResult> {
        if a.entity_type.as_str() != b.entity_type.as_str() {
            return Some(MatchResult {
                score: 0.0,
                is_match: false,
                reason: format!(
                    "Different types: {} vs {}",
                    a.entity_type.as_str(),
                    b.entity_type.as_str()
                ),
            });
        }

        let a_norm = normalize_label(&a.label);
        let b_norm = normalize_label(&b.label);
        let lexical = levenshtein_similarity(&a_norm, &b_norm);

        let a_text = build_entity_text(a);
        let b_text = build_entity_text(b);
        let semantic = jaccard_text_similarity(&a_text, &b_text);

        let heuristic = common_word_score(&a_norm, &b_norm);

        let score = lexical * 0.30 + semantic * 0.50 + heuristic * 0.20;
        let is_match = score >= self.match_threshold;

        Some(MatchResult {
            score,
            is_match,
            reason: if is_match {
                format!(
                    "Match: lexical={lexical:.2}, semantic={semantic:.2}, heuristic={heuristic:.2}",
                )
            } else {
                format!(
                    "No match: lexical={lexical:.2}, semantic={semantic:.2}, heuristic={heuristic:.2}",
                )
            },
        })
    }

    // ── Phase 3: Run full resolution ────────────────────────────────────

    /// Run all 3 phases against the knowledge graph.
    pub fn resolve(&self, kg: &mut KnowledgeGraph) -> ResolveStats {
        let entities = kg.all_entities();
        let pairs = self.find_candidate_pairs(&entities);

        let mut stats = ResolveStats {
            pairs_checked: pairs.len(),
            ..Default::default()
        };

        for (a, b) in &pairs {
            if let Some(result) = self.match_pair(a, b) {
                if result.is_match {
                    stats.matches_found += 1;
                    let (canonical, merged) = if a.created_at <= b.created_at {
                        (a.id.clone(), b.id.clone())
                    } else {
                        (b.id.clone(), a.id.clone())
                    };
                    kg.merge_entities(&canonical, &merged);
                    stats.entities_merged += 1;
                }
            }
        }

        stats
    }
}

// ── Entity Resolution Helpers ───────────────────────────────────────────────

fn normalize_label(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_prefix(s: &str, n: usize) -> String {
    normalize_label(s)
        .chars()
        .take(n)
        .collect::<String>()
        .to_lowercase()
}

fn build_entity_text(e: &Entity) -> String {
    let mut text = e.label.clone();
    for (k, v) in &e.properties {
        text.push(' ');
        text.push_str(k);
        text.push(' ');
        text.push_str(v);
    }
    text.to_lowercase()
}

fn levenshtein_similarity(a: &str, b: &str) -> f32 {
    let dist = levenshtein_distance(a, b);
    let max_len = a.len().max(b.len()).max(1) as f32;
    (1.0 - dist as f32 / max_len).max(0.0)
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

fn jaccard_text_similarity(a: &str, b: &str) -> f32 {
    let a_words: Vec<&str> = a
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .collect();
    let b_words: Vec<&str> = b
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .collect();

    if a_words.is_empty() && b_words.is_empty() {
        return 0.0;
    }

    let intersection = a_words.iter().filter(|w| b_words.contains(w)).count();
    let union = a_words.len() + b_words.len() - intersection;

    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

fn common_word_score(a: &str, b: &str) -> f32 {
    let a_words: Vec<&str> = a
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .collect();
    let b_words: Vec<&str> = b
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .collect();

    if a_words.is_empty() || b_words.is_empty() {
        return 0.0;
    }

    let common = a_words.iter().filter(|w| b_words.contains(w)).count();
    let max_common = a_words.len().min(b_words.len());

    if max_common == 0 {
        0.0
    } else {
        (common as f32 / max_common as f32).min(1.0)
    }
}
