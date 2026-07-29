//! Fact Manager — manages the facts/ directory (long-term memory).
//!
//! ## Dual-Write Architecture
//!
//! Facts are written to TWO places:
//!   1. **MD files** (`data/memory/facts/*.md`) — human-readable, Git-trackable
//!   2. **SQLite FTS5** (`everevo.db`, `facts` table) — sub-millisecond search
//!
//! ## Design Reference
//! - OpenDB (2025): SQLite+FTS5 achieves 93.6% LongMemEval, 0.5ms median retrieval
//! - Search is ×393 faster than vector-only approaches

use std::path::{Path, PathBuf};
use std::sync::Arc;

use everevo_core::memory::MemoryFact;
use everevo_core::EverEvoError;

use super::frontmatter::{parse_fact_file, serialize_fact_file};
use super::index::{load_all_facts, regenerate_index};

/// Manages the facts directory (data/memory/facts/).
///
/// ## Triple-Write Architecture
/// Facts are written to THREE places:
///   1. **MD files** (FactManager) — human-readable source of truth
///   2. **SQLite FTS5** (everevo.db, `facts` table) — sub-millisecond keyword search
///   3. **Vector store** (RagPipeline) — semantic search index (if configured)
pub struct FactManager {
    facts_dir: PathBuf,
    index_path: PathBuf,
    max_facts: usize,
    /// Optional RAG pipeline for real-time vector indexing on save.
    rag: Arc<std::sync::Mutex<Option<Arc<crate::rag::RagPipeline>>>>,
    /// Optional DB handle for SQLite FTS5 indexing on save.
    db: Arc<std::sync::Mutex<Option<Arc<everevo_db::Database>>>>,
}

impl FactManager {
    /// Create a new fact manager. Creates facts dir if missing.
    pub fn new(facts_dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let facts_dir: PathBuf = facts_dir.into();
        std::fs::create_dir_all(&facts_dir)
            .map_err(|e| EverEvoError::Internal(format!("Failed to create facts dir: {e}")))?;
        let index_path = facts_dir.parent().unwrap_or(&facts_dir).join("MEMORY.md");
        Ok(Self {
            facts_dir,
            index_path,
            max_facts: 200,
            rag: Arc::new(std::sync::Mutex::new(None)),
            db: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub fn with_max_facts(mut self, max: usize) -> Self {
        self.max_facts = max;
        self
    }

    /// Attach a RAG pipeline for real-time vector indexing on each save.
    pub fn set_rag(&self, rag: Arc<crate::rag::RagPipeline>) {
        if let Ok(mut guard) = self.rag.lock() {
            *guard = Some(rag);
        }
    }

    /// Attach a Database handle for SQLite FTS5 indexing on each save.
    pub fn set_db(&self, db: Arc<everevo_db::Database>) {
        if let Ok(mut guard) = self.db.lock() {
            // Ensure FTS5 table exists on first use
            *guard = Some(db);
        }
    }

    /// Save a fact to disk, regenerate the index, and auto-index into RAG.
    pub fn save(&self, fact: &MemoryFact) -> Result<(), EverEvoError> {
        let existing = load_all_facts(&self.facts_dir)?;
        let is_update = existing.iter().any(|f| f.name == fact.name);

        // Dedup check (Mem0 pattern: top-K similarity before ADD)
        if !is_update {
            // Build word set for the new fact
            let new_text = format!("{} {}", fact.description, fact.content).to_lowercase();
            let new_words: std::collections::HashSet<&str> = new_text
                .split_whitespace().filter(|w| w.len() > 2).collect();

            for old_fact in &existing {
                let old_text = format!("{} {}", old_fact.description, old_fact.content).to_lowercase();
                let old_words: std::collections::HashSet<&str> = old_text
                    .split_whitespace().filter(|w| w.len() > 2).collect();
                let intersection = new_words.intersection(&old_words).count();
                let union = new_words.len() + old_words.len() - intersection;
                if union > 0 {
                    let jaccard = intersection as f32 / union as f32;
                    if jaccard > 0.85 {
                        tracing::info!(
                            name = %fact.name,
                            existing = %old_fact.name,
                            jaccard,
                            "Dedup: similar fact exists, skipping save"
                        );
                        return Err(EverEvoError::InvalidInput(format!(
                            "Similar fact already exists: '{}' (Jaccard={:.2}). Use UPDATE if you want to modify it.",
                            old_fact.name, jaccard
                        )));
                    }
                }
            }

            if existing.len() >= self.max_facts {
                return Err(EverEvoError::InvalidInput(format!(
                    "Fact limit reached ({}). Consolidation required before adding new facts.",
                    self.max_facts
                )));
            }
        }

        let path = self.fact_path(&fact.name);
        let content = serialize_fact_file(fact);
        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Failed to write fact: {e}")))?;

        regenerate_index(&self.facts_dir, &self.index_path)?;

        // SQLite FTS5 indexing (keyword search, sub-millisecond)
        if let Ok(guard) = self.db.lock() {
            if let Some(ref db) = *guard {
                let content = format!("{}: {}", fact.name, fact.content);
                let db = Arc::clone(db);
                let name = fact.name.clone();
                let desc = fact.description.clone();
                // Fire-and-forget: don't block the save for SQLite write
                tokio::spawn(async move {
                    if let Err(e) = db.upsert_fact(&name, &desc, &content, "project").await {
                        tracing::warn!(error = %e, "Fact SQLite indexing failed");
                    }
                });
            }
        }

        // Real-time vector indexing
        if let Ok(guard) = self.rag.lock() {
            if let Some(ref rag) = *guard {
                let chunk = crate::rag::make_chunk(
                    format!("{}: {}", fact.name, fact.content),
                    everevo_vector::ChunkType::Fact,
                    fact.projection.source_pointers.clone(),
                );
                if let Err(e) = rag.ingest(vec![chunk]) {
                    tracing::warn!(error = %e, "Fact vector indexing failed");
                }
            }
        }

        tracing::info!(name = %fact.name, updated = is_update, "Fact saved");
        Ok(())
    }

    /// Load a single fact by name.
    pub fn load(&self, name: &str) -> Result<Option<MemoryFact>, EverEvoError> {
        let path = self.fact_path(name);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| EverEvoError::Internal(format!("Read fact: {e}")))?;
        Ok(parse_fact_file(name, &content))
    }

    /// Load all facts.
    pub fn load_all(&self) -> Result<Vec<MemoryFact>, EverEvoError> {
        load_all_facts(&self.facts_dir)
    }

    /// Delete a fact by name.
    pub fn delete(&self, name: &str) -> Result<(), EverEvoError> {
        let path = self.fact_path(name);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| EverEvoError::Internal(format!("Delete fact: {e}")))?;
            regenerate_index(&self.facts_dir, &self.index_path)?;
        }
        Ok(())
    }

    /// Count total facts.
    pub fn count(&self) -> Result<usize, EverEvoError> {
        Ok(load_all_facts(&self.facts_dir)?.len())
    }

    /// Bump the recall count for a fact (called when MemoryStage injects it).
    /// When recall ≥ 3, the fact is promoted to T1 (bootstrap injection).
    pub fn bump_recall(&self, name: &str) -> Result<(), EverEvoError> {
        let path = self.fact_path(name);
        if !path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| EverEvoError::Internal(format!("Read fact for bump: {e}")))?;
        let (fm, body) = super::frontmatter::parse_frontmatter(&content)
            .map(|(fm, b)| (fm, b.to_string()))
            .unwrap_or_default();

        let current_recall: u32 = super::frontmatter::get_recall(&fm);
        let new_recall = current_recall.saturating_add(1);
        let tier: u8 = if new_recall >= 3 { 1 } else { super::frontmatter::get_tier(&fm) };

        // Reconstruct the file with updated recall
        let mut out = String::new();
        out.push_str("---\n");
        for (k, v) in &fm {
            if k == "recall" {
                out.push_str(&format!("recall: {new_recall}\n"));
            } else if k == "tier" && new_recall >= 3 {
                out.push_str("tier: 1\n");
            } else {
                out.push_str(&format!("{k}: {v}\n"));
            }
        }
        if !fm.contains_key("recall") {
            out.push_str(&format!("recall: {new_recall}\n"));
        }
        if !fm.contains_key("tier") {
            out.push_str(&format!("tier: {tier}\n"));
        }
        out.push_str("---\n\n");
        out.push_str(&body);

        std::fs::write(&path, &out)
            .map_err(|e| EverEvoError::Internal(format!("Write bumped fact: {e}")))?;

        if new_recall == 3 {
            tracing::info!(name, "Fact promoted to T1 (recall >= 3)");
        }
        Ok(())
    }

    /// Load T1 facts (recall ≥ 3, high-frequency bootstrap facts).
    /// These are injected at session start (Claude Code T1 pattern).
    pub fn load_tier1(&self) -> Result<Vec<MemoryFact>, EverEvoError> {
        let all = load_all_facts(&self.facts_dir)?;
        Ok(all.into_iter().filter(|f| {
            let path = self.fact_path(&f.name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some((fm, _)) = super::frontmatter::parse_frontmatter(&content) {
                    return super::frontmatter::get_tier(&fm) <= 1;
                }
            }
            false
        }).collect())
    }

    /// Read the MEMORY.md index (first 300 lines for context injection).
    pub fn read_index_lean(&self, max_lines: usize) -> Result<String, EverEvoError> {
        if !self.index_path.exists() {
            return Ok(String::new());
        }
        let content = std::fs::read_to_string(&self.index_path)
            .map_err(|e| EverEvoError::Internal(format!("Read index: {e}")))?;
        Ok(content
            .lines()
            .take(max_lines)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Path to a specific fact file.
    pub fn fact_path(&self, name: &str) -> PathBuf {
        self.facts_dir.join(format!("{name}.md"))
    }

    /// Facts directory.
    pub fn facts_dir(&self) -> &Path {
        &self.facts_dir
    }

    /// Index path.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Index all stored facts into a RAG pipeline for semantic search.
    ///
    /// Each fact's content is embedded and stored as a vector chunk.
    pub fn index_into_rag(&self, rag: &crate::rag::RagPipeline) -> Result<usize, EverEvoError> {
        let facts = self.load_all()?;
        let mut count = 0usize;
        for fact in &facts {
            let chunk = crate::rag::make_chunk(
                format!("{}: {}", fact.name, fact.content),
                everevo_vector::ChunkType::Fact,
                fact.projection.source_pointers.clone(),
            );
            rag.ingest(vec![chunk])?;
            count += 1;
        }
        tracing::info!(count, "Memory facts indexed into RAG");
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::memory::{FactType, ProjectionMetadata};
    use tempfile::TempDir;

    #[test]
    fn test_save_and_load() {
        let dir = TempDir::new().unwrap();
        let mgr = FactManager::new(dir.path()).unwrap();

        let fact = MemoryFact {
            name: "test-pref".into(),
            description: "A test preference".into(),
            content: "Always test".into(),
            fact_type: FactType::User,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
        };

        mgr.save(&fact).unwrap();
        let loaded = mgr.load("test-pref").unwrap().unwrap();
        assert_eq!(loaded.content, "Always test");

        let count = mgr.count().unwrap();
        assert_eq!(count, 1);
    }
}
