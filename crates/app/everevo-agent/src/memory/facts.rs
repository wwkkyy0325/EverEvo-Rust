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
use uuid::Uuid;

use super::frontmatter::{parse_fact_file, serialize_fact_file};
use super::index::{load_all_facts, regenerate_index};

/// A fact upsert task enqueued to the serialized FTS5 writer actor.
///
/// The writer processes these one at a time, avoiding concurrent writes to the
/// FTS5 external-content table — the root cause of "SQL logic error" (code 1)
/// that surfaced when multiple facts were saved within the same millisecond.
#[derive(Debug, Clone)]
pub struct FactWriteTask {
    pub id: String,
    pub description: String,
    pub content: String,
    pub fact_type: String,
}

/// Sender for the serialized fact-writer actor.
pub type FactWriteTx = tokio::sync::mpsc::UnboundedSender<FactWriteTask>;

/// Whether a fact is visible to the given session's recall (分层记忆 scoping).
///
/// Untagged (`None`, legacy) and `"global"` facts are cross-session long-term
/// memory — visible to every session. A `Some(uuid)` fact is session-scoped
/// working memory — strictly isolated, visible only to its own session.
pub fn fact_visible_to(fact: &MemoryFact, session_id: Option<&Uuid>) -> bool {
    match fact.session.as_deref() {
        None | Some("global") => true,
        Some(owner) => session_id.is_some_and(|sid| sid.to_string() == owner),
    }
}

/// Manages the facts directory (data/memory/facts/).
///
/// ## Triple-Write Architecture
/// Facts are written to THREE places:
///   1. **MD files** (FactManager) — human-readable source of truth
///   2. **SQLite FTS5** (everevo.db, `facts` table) — sub-millisecond keyword search
///   3. **Vector store** (RagPipeline) — semantic search index (if configured)
#[derive(Clone)]
pub struct FactManager {
    facts_dir: PathBuf,
    index_path: PathBuf,
    max_facts: usize,
    /// Optional RAG pipeline for real-time vector indexing on save.
    /// Set once during init, read-only thereafter — `OnceLock` avoids lock overhead.
    rag: Arc<std::sync::OnceLock<Arc<crate::rag::RagPipeline>>>,
    /// Optional DB handle for SQLite FTS5 indexing on save.
    db: Arc<std::sync::OnceLock<Arc<everevo_db::Database>>>,
    /// Serialized FTS5 writer channel. When set, fact upserts are enqueued here
    /// instead of fire-and-forget spawned — eliminates concurrent FTS5
    /// external-content trigger conflicts ("SQL logic error").
    write_queue: Arc<std::sync::OnceLock<FactWriteTx>>,
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
            rag: Arc::new(std::sync::OnceLock::new()),
            db: Arc::new(std::sync::OnceLock::new()),
            write_queue: Arc::new(std::sync::OnceLock::new()),
        })
    }

    pub fn with_max_facts(mut self, max: usize) -> Self {
        self.max_facts = max;
        self
    }

    /// Attach a RAG pipeline for real-time vector indexing on each save.
    pub fn set_rag(&self, rag: Arc<crate::rag::RagPipeline>) {
        let _ = self.rag.set(rag);
    }

    /// Attach a Database handle for SQLite FTS5 indexing on each save.
    pub fn set_db(&self, db: Arc<everevo_db::Database>) {
        let _ = self.db.set(db);
    }

    /// Attach a serialized writer channel. When set, `save()` enqueues FTS5
    /// upserts here instead of spawning unbounded tasks — eliminates the
    /// concurrent FTS5 external-content trigger conflicts that produced
    /// "SQL logic error" (code 1) under burst saves.
    pub fn set_write_queue(&self, tx: FactWriteTx) {
        let _ = self.write_queue.set(tx);
    }

    /// Save a fact to disk, regenerate the index, and auto-index into RAG.
    ///
    /// Synchronous — uses `std::fs` directly. For async contexts, prefer
    /// [`save_async`] which wraps the entire save+index pipeline in `spawn_blocking`
    /// to avoid blocking the tokio runtime thread.
    pub fn save(&self, fact: &MemoryFact) -> Result<(), EverEvoError> {
        let existing = load_all_facts(&self.facts_dir)?;
        let is_update = existing.iter().any(|f| f.name == fact.name);

        // Dedup check (Mem0 pattern: top-K similarity before ADD)
        if !is_update {
            // Build word set for the new fact
            let new_text = format!("{} {}", fact.description, fact.content).to_lowercase();
            let new_words: std::collections::HashSet<&str> = new_text
                .split_whitespace()
                .filter(|w| w.len() > 2)
                .collect();

            for old_fact in &existing {
                let old_text =
                    format!("{} {}", old_fact.description, old_fact.content).to_lowercase();
                let old_words: std::collections::HashSet<&str> = old_text
                    .split_whitespace()
                    .filter(|w| w.len() > 2)
                    .collect();
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

        // SQLite FTS5 indexing (keyword search, sub-millisecond).
        // Prefer the serialized writer queue to avoid concurrent FTS5
        // external-content trigger conflicts ("SQL logic error"). Falls back
        // to a fire-and-forget spawn when no queue is attached (e.g. tests).
        if let Some(db) = self.db.get() {
            let task = FactWriteTask {
                id: fact.name.clone(),
                description: fact.description.clone(),
                content: format!("{}: {}", fact.name, fact.content),
                fact_type: "project".to_string(),
            };
            let queued = self
                .write_queue
                .get()
                .map(|tx| tx.send(task.clone()).is_ok())
                .unwrap_or(false);
            if !queued {
                let db = Arc::clone(db);
                tokio::spawn(async move {
                    if let Err(e) = db
                        .upsert_fact(&task.id, &task.description, &task.content, &task.fact_type)
                        .await
                    {
                        tracing::warn!(error = %e, "Fact SQLite indexing failed");
                    }
                });
            }
        }

        // Real-time vector indexing
        if let Some(rag) = self.rag.get() {
            let chunk = crate::rag::make_chunk_with_sources(
                format!("{}: {}", fact.name, fact.content),
                everevo_vector::ChunkType::Fact,
                fact.projection.source_pointers.clone(),
            );
            if let Err(e) = rag.ingest_into("memory", vec![chunk]) {
                tracing::warn!(error = %e, "Fact vector indexing failed");
            }
        }

        tracing::info!(name = %fact.name, updated = is_update, "Fact saved");
        Ok(())
    }

    /// Async wrapper — runs the full save+index+vector pipeline on the blocking
    /// thread pool so the tokio runtime stays responsive under concurrent saves.
    pub async fn save_async(&self, fact: MemoryFact) -> Result<(), EverEvoError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.save(&fact))
            .await
            .map_err(|e| EverEvoError::Internal(format!("Fact save panicked: {e}")))?
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
        let tier: u8 = if new_recall >= 3 {
            1
        } else {
            super::frontmatter::get_tier(&fm)
        };

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
        Ok(all
            .into_iter()
            .filter(|f| {
                let path = self.fact_path(&f.name);
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some((fm, _)) = super::frontmatter::parse_frontmatter(&content) {
                        return super::frontmatter::get_tier(&fm) <= 1;
                    }
                }
                false
            })
            .collect())
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
            let chunk = crate::rag::make_chunk_with_sources(
                format!("{}: {}", fact.name, fact.content),
                everevo_vector::ChunkType::Fact,
                fact.projection.source_pointers.clone(),
            );
            rag.ingest_into("memory", vec![chunk])?;
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
            session: None,
        };

        mgr.save(&fact).unwrap();
        let loaded = mgr.load("test-pref").unwrap().unwrap();
        assert_eq!(loaded.content, "Always test");

        let count = mgr.count().unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_session_tag_roundtrips_through_frontmatter() {
        let dir = TempDir::new().unwrap();
        let mgr = FactManager::new(dir.path()).unwrap();

        let sid = uuid::Uuid::new_v4().to_string();
        let fact = MemoryFact {
            name: "session-fact".into(),
            description: "Session working memory".into(),
            content: "Only this session should see this".into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
            session: Some(sid.clone()),
        };
        mgr.save(&fact).unwrap();
        let loaded = mgr.load("session-fact").unwrap().unwrap();
        assert_eq!(loaded.session.as_deref(), Some(sid.as_str()));
    }

    #[test]
    fn test_fact_visible_to_scoping() {
        let sess_a = uuid::Uuid::new_v4();
        let sess_b = uuid::Uuid::new_v4();

        let legacy = MemoryFact {
            session: None,
            ..fact_skeleton("legacy")
        };
        let global = MemoryFact {
            session: Some("global".into()),
            ..fact_skeleton("global")
        };
        let owned_a = MemoryFact {
            session: Some(sess_a.to_string()),
            ..fact_skeleton("owned-a")
        };

        // Legacy (untagged) + explicit global = visible to every session.
        assert!(fact_visible_to(&legacy, Some(&sess_b)));
        assert!(fact_visible_to(&global, Some(&sess_b)));
        // Owned facts are visible only to their own session.
        assert!(fact_visible_to(&owned_a, Some(&sess_a)));
        assert!(!fact_visible_to(&owned_a, Some(&sess_b)));
        // Without a session context, only global facts are visible.
        assert!(fact_visible_to(&global, None));
        assert!(!fact_visible_to(&owned_a, None));
    }

    fn fact_skeleton(name: &str) -> MemoryFact {
        MemoryFact {
            name: name.into(),
            description: "d".into(),
            content: "c".into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
            session: None,
        }
    }

    /// When a serialized writer queue is attached, `save()` must enqueue the
    /// upsert task to the queue rather than spawning a fire-and-forget task.
    /// This is the core of the fix for concurrent FTS5 "SQL logic error".
    #[tokio::test]
    async fn test_save_enqueues_via_writer_queue() {
        let dir = TempDir::new().unwrap();
        let mgr = FactManager::new(dir.path()).unwrap();

        // Attach an in-memory DB so save() enters the SQLite-indexing branch.
        let db = everevo_db::Database::connect(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        mgr.set_db(Arc::new(db));

        // Attach the serialized writer queue.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        mgr.set_write_queue(tx);

        let fact = MemoryFact {
            name: "queued-fact".into(),
            description: "Queue test".into(),
            content: "This fact should be enqueued, not spawned".into(),
            fact_type: FactType::Project,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("test", "none", vec![], 1.0),
            links: vec![],
            session: None,
        };
        mgr.save(&fact).unwrap();

        // The task must arrive on the queue (not via tokio::spawn).
        let task = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for queued task")
            .expect("channel closed unexpectedly");

        assert_eq!(task.id, "queued-fact");
        assert_eq!(task.description, "Queue test");
        assert_eq!(task.fact_type, "project");
        assert!(task.content.starts_with("queued-fact:"));
    }
}
