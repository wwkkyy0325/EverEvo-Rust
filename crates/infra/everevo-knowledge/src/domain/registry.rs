//! Domain registry — named collections of related documents with centroid-based classification.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::helpers::cosine_similarity;
use everevo_core::EverEvoError;

// ── Domain ────────────────────────────────────────────────────────────────

/// A knowledge domain — a named collection of related documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    /// Unique kebab-case slug.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// LLM-generated description of what this domain covers.
    pub description: String,
    /// Centroid vector of all documents in this domain (for auto-classification).
    /// Persisted across restarts so classification survives.
    pub centroid: Vec<f32>,
    /// Parent domain (for split hierarchies).
    pub parent_id: Option<String>,
    /// Related domains (cross-domain links).
    pub related_ids: Vec<String>,
    /// If merged into another domain.
    pub merged_into: Option<String>,
    /// Number of documents.
    pub document_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Domain Registry ───────────────────────────────────────────────────────

/// Registry of all domains, persisted as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRegistry {
    pub domains: HashMap<String, Domain>,
    /// Embedding dimension (must match fastembed-rs).
    pub embedding_dim: usize,
}

impl DomainRegistry {
    pub fn load(path: &Path) -> Result<Self, EverEvoError> {
        if path.exists() {
            let json = std::fs::read_to_string(path)
                .map_err(|e| EverEvoError::Internal(format!("Read registry: {e}")))?;
            Ok(serde_json::from_str(&json).unwrap_or(Self {
                domains: HashMap::new(),
                embedding_dim: 384,
            }))
        } else {
            Ok(Self {
                domains: HashMap::new(),
                embedding_dim: 384,
            })
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), EverEvoError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| EverEvoError::Internal(format!("Save registry: {e}")))?;
        std::fs::write(path, &json)
            .map_err(|e| EverEvoError::Internal(format!("Write registry: {e}")))?;
        Ok(())
    }

    /// Find the best-matching domain for a document vector.
    /// Returns (domain_id, similarity_score).
    pub fn classify(&self, doc_vector: &[f32]) -> (Option<String>, f32) {
        let mut best_id = None;
        let mut best_sim = 0.0_f32;

        for (id, domain) in &self.domains {
            if domain.merged_into.is_some() || domain.centroid.is_empty() {
                continue;
            }
            let sim = cosine_similarity(doc_vector, &domain.centroid);
            if sim > best_sim {
                best_sim = sim;
                best_id = Some(id.clone());
            }
        }
        (best_id, best_sim)
    }

    /// Update a domain's centroid after adding a new document.
    pub fn update_centroid(&mut self, domain_id: &str, new_vec: &[f32]) {
        if let Some(d) = self.domains.get_mut(domain_id) {
            let n = d.document_count as f32;
            if d.centroid.is_empty() {
                d.centroid = new_vec.to_vec();
            } else {
                for (c, v) in d.centroid.iter_mut().zip(new_vec.iter()) {
                    *c = (*c * n + v) / (n + 1.0);
                }
            }
            d.document_count += 1;
            d.updated_at = Utc::now();
        }
    }

    /// Create a new domain.
    pub fn create(&mut self, id: String, name: String, description: String) {
        let domain_id = id.clone();
        self.domains.insert(
            domain_id,
            Domain {
                id,
                name,
                description,
                centroid: vec![],
                parent_id: None,
                related_ids: vec![],
                merged_into: None,
                document_count: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
    }

    /// Merge `source_id` into `target_id`. All documents, relations, and centroid
    /// move to the target. Source is marked merged_into (never deleted).
    pub fn merge_domains(&mut self, target_id: &str, source_id: &str) -> Result<(), EverEvoError> {
        if target_id == source_id {
            return Err(EverEvoError::InvalidInput(
                "Cannot merge a domain into itself".into(),
            ));
        }
        let source = self
            .domains
            .get(source_id)
            .ok_or_else(|| EverEvoError::NotFound(source_id.into()))?;
        let doc_count = source.document_count;
        let source_centroid = source.centroid.clone();
        let source_relations = source.related_ids.clone();

        // Transfer document count to target
        if let Some(target) = self.domains.get_mut(target_id) {
            target.document_count += doc_count;
            // Merge centroids
            if !target.centroid.is_empty() && !source_centroid.is_empty() {
                let tn = (target.document_count - doc_count) as f32;
                let sn = doc_count as f32;
                for (tc, sc) in target.centroid.iter_mut().zip(source_centroid.iter()) {
                    *tc = (*tc * tn + sc * sn) / (tn + sn);
                }
            } else if target.centroid.is_empty() {
                target.centroid = source_centroid;
            }
            // Merge relations
            for rel in &source_relations {
                if rel != target_id && !target.related_ids.contains(rel) {
                    target.related_ids.push(rel.clone());
                }
            }
            target.updated_at = Utc::now();
        }

        // Collect reverse relations that need updating (avoid double-mut-borrow)
        let mut reverse_updates: Vec<String> = Vec::new();
        for (id, d) in self.domains.iter() {
            if d.related_ids.contains(&source_id.to_string()) && id != target_id {
                reverse_updates.push(id.clone());
            }
        }
        for id in &reverse_updates {
            if let Some(d) = self.domains.get_mut(id) {
                d.related_ids.retain(|r| r != source_id);
                if !d.related_ids.contains(&target_id.to_string()) {
                    d.related_ids.push(target_id.to_string());
                }
                d.updated_at = Utc::now();
            }
        }

        // Mark source as merged
        if let Some(source) = self.domains.get_mut(source_id) {
            source.merged_into = Some(target_id.to_string());
            source.updated_at = Utc::now();
        }

        Ok(())
    }

    /// Add a cross-domain relationship.
    pub fn add_relation(
        &mut self,
        from_id: &str,
        to_id: &str,
        _relation_type: &str,
    ) -> Result<(), EverEvoError> {
        if let Some(d) = self.domains.get_mut(from_id) {
            if !d.related_ids.contains(&to_id.to_string()) {
                d.related_ids.push(to_id.to_string());
                d.updated_at = Utc::now();
            }
        }
        // Symmetric: also add reverse
        if let Some(d) = self.domains.get_mut(to_id) {
            if !d.related_ids.contains(&from_id.to_string()) {
                d.related_ids.push(from_id.to_string());
                d.updated_at = Utc::now();
            }
        }
        Ok(())
    }

    /// Register a document in a domain (increment count, update centroid).
    pub fn add_document(
        &mut self,
        domain_id: &str,
        doc_vector: &[f32],
    ) -> Result<(), EverEvoError> {
        if !self.domains.contains_key(domain_id) {
            return Err(EverEvoError::NotFound(format!(
                "Domain not found: {domain_id}"
            )));
        }
        self.update_centroid(domain_id, doc_vector);
        Ok(())
    }

    /// Find or suggest related domains based on centroid similarity.
    pub fn suggest_relations(&self, domain_id: &str, threshold: f32) -> Vec<(String, f32)> {
        let source = match self.domains.get(domain_id) {
            Some(d) if !d.centroid.is_empty() => d,
            _ => return vec![],
        };
        let mut related = Vec::new();
        for (id, d) in &self.domains {
            if id == domain_id || d.centroid.is_empty() || d.merged_into.is_some() {
                continue;
            }
            let sim = cosine_similarity(&source.centroid, &d.centroid);
            if sim > threshold {
                related.push((id.clone(), sim));
            }
        }
        related.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        related
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_domain_registry_create_and_classify() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();

        reg.create("rust".into(), "Rust".into(), "Rust programming".into());
        reg.update_centroid("rust", &vec![1.0_f32; 384]);

        let (id, sim) = reg.classify(&vec![1.0_f32; 384]);
        assert_eq!(id.unwrap(), "rust");
        assert!(sim > 0.9);
    }

    #[test]
    fn test_domain_registry_save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("test".into(), "Test".into(), "A test domain".into());
        reg.save(&path).unwrap();

        let loaded = DomainRegistry::load(&path).unwrap();
        assert!(loaded.domains.contains_key("test"));
    }

    #[test]
    fn test_centroid_update_formula() {
        // Incremental centroid should match the mathematical running average.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("math".into(), "Math".into(), "Math domain".into());

        // Add 3 identical vectors
        let v = vec![0.5_f32; 384];
        reg.update_centroid("math", &v);
        reg.update_centroid("math", &v);
        reg.update_centroid("math", &v);

        let domain = &reg.domains["math"];
        assert_eq!(domain.document_count, 3);
        // Centroid of identical vectors should be the same vector (within float tolerance)
        for &c in &domain.centroid {
            assert!(
                (c - 0.5).abs() < 1e-6,
                "centroid element should be ~0.5, got {c}"
            );
        }
    }

    #[test]
    fn test_centroid_update_mixed() {
        // Verify centroid is the correct average of different vectors.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("mix".into(), "Mix".into(), "Mixed domain".into());

        // First vector: all 0.0
        let v1 = vec![0.0_f32; 384];
        reg.update_centroid("mix", &v1);
        // Second vector: all 1.0
        let v2 = vec![1.0_f32; 384];
        reg.update_centroid("mix", &v2);

        let domain = &reg.domains["mix"];
        assert_eq!(domain.document_count, 2);
        // Average of [0,0,...] and [1,1,...] = [0.5, 0.5, ...]
        for &c in &domain.centroid {
            assert!(
                (c - 0.5).abs() < 1e-6,
                "centroid element should be 0.5, got {c}"
            );
        }
    }

    #[test]
    fn test_merge_domains_centroid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("target".into(), "Target".into(), "Target domain".into());
        reg.create("source".into(), "Source".into(), "Source domain".into());

        reg.update_centroid("target", &vec![0.0_f32; 384]);
        reg.update_centroid("target", &vec![0.0_f32; 384]); // target: 2 docs, centroid=[0.0]
        reg.update_centroid("source", &vec![1.0_f32; 384]); // source: 1 doc, centroid=[1.0]

        reg.merge_domains("target", "source").unwrap();

        let target = &reg.domains["target"];
        assert_eq!(target.document_count, 3);
        // Weighted average: (2*0.0 + 1*1.0) / 3 = 0.333...
        for &c in &target.centroid {
            assert!(
                (c - 1.0 / 3.0).abs() < 1e-5,
                "merged centroid should be ~0.333, got {c}"
            );
        }
    }

    #[test]
    fn test_merge_domains_self_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("solo".into(), "Solo".into(), "Solo domain".into());

        let result = reg.merge_domains("solo", "solo");
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_nonexistent_source_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("target".into(), "Target".into(), "Target".into());

        let result = reg.merge_domains("target", "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_classify_empty_registry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let reg = DomainRegistry::load(&path).unwrap();
        let (id, sim) = reg.classify(&vec![1.0_f32; 384]);
        assert!(id.is_none());
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_classify_skips_merged_domain() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("a".into(), "A".into(), "Domain A".into());
        reg.create("b".into(), "B".into(), "Domain B".into());
        reg.update_centroid("a", &vec![1.0_f32; 384]);
        reg.update_centroid("b", &vec![0.5_f32; 384]);
        reg.merge_domains("a", "b").unwrap();

        // b is now merged_into a; classify should NOT return b
        let (id, _) = reg.classify(&vec![0.5_f32; 384]);
        assert_eq!(id.unwrap(), "a"); // only a is active
    }

    #[test]
    fn test_suggest_relations_threshold() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("rust".into(), "Rust".into(), "Rust lang".into());
        reg.create("python".into(), "Python".into(), "Python lang".into());
        reg.create("cooking".into(), "Cooking".into(), "Cooking recipes".into());
        reg.update_centroid("rust", &vec![1.0_f32; 384]);
        reg.update_centroid("python", &vec![0.9_f32; 384]); // similar to rust
        reg.update_centroid("cooking", &vec![0.0_f32; 384]); // dissimilar

        // High threshold: only very similar domains
        let related = reg.suggest_relations("rust", 0.95);
        assert!(related.iter().any(|(id, _)| id == "python"));
        assert!(!related.iter().any(|(id, _)| id == "cooking"));
    }

    #[test]
    fn test_add_relation_symmetric() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        reg.create("a".into(), "A".into(), "Domain A".into());
        reg.create("b".into(), "B".into(), "Domain B".into());

        reg.add_relation("a", "b", "related").unwrap();
        assert!(reg.domains["a"].related_ids.contains(&"b".to_string()));
        assert!(reg.domains["b"].related_ids.contains(&"a".to_string()));
    }

    #[test]
    fn test_add_document_to_nonexistent_domain() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("domains.json");
        let mut reg = DomainRegistry::load(&path).unwrap();
        let result = reg.add_document("nonexistent", &vec![1.0_f32; 384]);
        assert!(result.is_err());
    }
}
