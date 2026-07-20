//! Domain classifier — auto-classifies documents into domains based on embedding similarity.
//!
//! ## Anti-Fragmentation Rules
//!
//! 1. **Min docs for new domain**: Don't create a domain from a single doc.
//!    A new domain requires at least `min_docs_for_new_domain` unclassified
//!    documents with similar vectors before creation.
//! 2. **Long content detection**: Documents > `long_content_threshold` chars
//!    (novels, books, long papers) are always assigned to the closest existing
//!    domain, never create a new one — they're too unique to form a domain.
//! 3. **Manual lock**: Documents with `source=Manual` are never re-classified.

use super::registry::DomainRegistry;

/// Auto-classifies documents into domains based on embedding similarity.
pub struct DomainClassifier {
    /// Threshold: similarity above this → belongs to domain.
    pub high_threshold: f32,
    /// Threshold: similarity below this → NEW domain candidate.
    pub low_threshold: f32,
    /// Minimum number of documents before creating a new domain.
    /// Prevents single-document domains for novels/one-off files.
    pub min_docs_for_new_domain: usize,
    /// Documents longer than this (chars) never create new domains.
    /// They're creative/long-form content that's inherently unique.
    pub long_content_threshold: usize,
}

impl Default for DomainClassifier {
    fn default() -> Self {
        Self {
            high_threshold: 0.75,
            low_threshold: 0.45,
            min_docs_for_new_domain: 3,
            long_content_threshold: 10000,
        }
    }
}

impl DomainClassifier {
    /// Classify a document. `pending_count` = how many other unclassified
    /// docs have similar vectors (for min_docs threshold).
    pub fn classify(
        &self,
        registry: &DomainRegistry,
        doc_vector: &[f32],
        content_len: usize,
        pending_similar: usize,
    ) -> ClassificationResult {
        let (best_id, best_sim) = registry.classify(doc_vector);

        // Rule 1: High similarity → existing domain
        if best_sim > self.high_threshold {
            return ClassificationResult {
                domain_id: best_id.unwrap(),
                confidence: best_sim,
                is_new_domain: false,
                needs_llm: false,
                reason: format!("High similarity ({best_sim:.2})"),
            };
        }

        // Rule 2: Long content → suggest domain creation, never auto-assign.
        if content_len > self.long_content_threshold {
            return ClassificationResult {
                domain_id: String::new(),
                confidence: 0.0,
                is_new_domain: true,
                needs_llm: true,
                reason: format!(
                    "Long-form content ({content_len} chars). Suggest creating a dedicated domain. \
                     Create domain via POST /api/domains, then drop files into data/domain/{{name}}/inbox/"
                ),
            };
        }

        // Rule 3: Low similarity + enough pending docs → new domain candidate
        if best_sim < self.low_threshold && pending_similar >= self.min_docs_for_new_domain {
            return ClassificationResult {
                domain_id: String::new(),
                confidence: 1.0 - best_sim,
                is_new_domain: true,
                needs_llm: true,
                reason: format!(
                    "New domain candidate: {pending_similar} similar docs, low sim ({best_sim:.2})"
                ),
            };
        }

        // Rule 4: Grey area → assign to closest with LLM review
        ClassificationResult {
            domain_id: best_id.unwrap_or_else(|| "general".into()),
            confidence: best_sim.max(0.3),
            is_new_domain: false,
            needs_llm: best_sim < self.high_threshold && best_sim > self.low_threshold,
            reason: format!("Grey area ({best_sim:.2}) — assigned to closest domain"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub domain_id: String,
    pub confidence: f32,
    pub is_new_domain: bool,
    pub needs_llm: bool,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_classifier_anti_fragmentation() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("rust".into(), "Rust".into(), "desc".into());
        reg.update_centroid("rust", &vec![1.0_f32; 384]);

        let classifier = DomainClassifier::default();
        // Short doc with 3+ similar pending → new domain
        let result = classifier.classify(&reg, &vec![-1.0_f32; 384], 100, 3);
        assert!(result.is_new_domain, "3+ similar docs should trigger new domain");

        // Short doc with only 1 similar → assign to closest (anti-fragmentation)
        let result2 = classifier.classify(&reg, &vec![-1.0_f32; 384], 100, 1);
        assert!(
            !result2.is_new_domain,
            "Single doc should not create new domain"
        );

        // Long content (>10K chars) → suggest domain creation (novel/book case)
        let result3 = classifier.classify(&reg, &vec![-1.0_f32; 384], 15000, 5);
        assert!(
            result3.is_new_domain,
            "Long content should suggest dedicated domain"
        );
        assert!(
            result3.needs_llm,
            "Long content needs LLM to suggest domain name"
        );
    }
}
