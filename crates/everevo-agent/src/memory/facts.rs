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
/// ## Dual-Write Architecture
/// Facts are written to TWO places:
///   1. **MD files** (FactManager) — human-readable source of truth
///   2. **Vector store** (RagPipeline) — semantic search index (if configured)
/// The server coordinates both writes; FactManager handles the file side.
pub struct FactManager {
    facts_dir: PathBuf,
    index_path: PathBuf,
    max_facts: usize,
    /// Optional RAG pipeline for real-time vector indexing on save.
    rag: Arc<std::sync::Mutex<Option<Arc<crate::rag::RagPipeline>>>>,
}

impl FactManager {
    /// Create a new fact manager. Creates facts dir if missing.
    pub fn new(facts_dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let facts_dir: PathBuf = facts_dir.into();
        std::fs::create_dir_all(&facts_dir).map_err(|e| {
            EverEvoError::Internal(format!("Failed to create facts dir: {e}"))
        })?;
        let index_path = facts_dir.parent().unwrap_or(&facts_dir).join("MEMORY.md");
        Ok(Self {
            facts_dir,
            index_path,
            max_facts: 200,
            rag: Arc::new(std::sync::Mutex::new(None)),
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

    /// Save a fact to disk, regenerate the index, and auto-index into RAG.
    pub fn save(&self, fact: &MemoryFact) -> Result<(), EverEvoError> {
        let existing = load_all_facts(&self.facts_dir)?;
        let is_update = existing.iter().any(|f| f.name == fact.name);

        if !is_update && existing.len() >= self.max_facts {
            return Err(EverEvoError::InvalidInput(format!(
                "Fact limit reached ({}). Consolidation required before adding new facts.",
                self.max_facts
            )));
        }

        let path = self.fact_path(&fact.name);
        let content = serialize_fact_file(fact);
        std::fs::write(&path, &content).map_err(|e| {
            EverEvoError::Internal(format!("Failed to write fact: {e}"))
        })?;

        regenerate_index(&self.facts_dir, &self.index_path)?;

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
        let content = std::fs::read_to_string(&path).map_err(|e| {
            EverEvoError::Internal(format!("Read fact: {e}"))
        })?;
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
            std::fs::remove_file(&path).map_err(|e| {
                EverEvoError::Internal(format!("Delete fact: {e}"))
            })?;
            regenerate_index(&self.facts_dir, &self.index_path)?;
        }
        Ok(())
    }

    /// Count total facts.
    pub fn count(&self) -> Result<usize, EverEvoError> {
        Ok(load_all_facts(&self.facts_dir)?.len())
    }

    /// Read the MEMORY.md index (first 300 lines for context injection).
    pub fn read_index_lean(&self, max_lines: usize) -> Result<String, EverEvoError> {
        if !self.index_path.exists() {
            return Ok(String::new());
        }
        let content = std::fs::read_to_string(&self.index_path).map_err(|e| {
            EverEvoError::Internal(format!("Read index: {e}"))
        })?;
        Ok(content.lines().take(max_lines).collect::<Vec<_>>().join("\n"))
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
    pub fn index_into_rag(
        &self,
        rag: &crate::rag::RagPipeline,
    ) -> Result<usize, EverEvoError> {
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
