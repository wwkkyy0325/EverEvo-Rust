//! Memory types — shared across everevo-core, everevo-agent, everevo-db.
//!
//! ## Design Principle
//!
//! Every derived memory artifact is a PROJECTION of the immutable raw log.
//! The source pointers guarantee bidirectional traceability:
//!   SQLite message ← SourcePointer → Vector chunk → Graph entity → Wiki page
//!
//! ## Reference
//!
//! TierMem (arXiv:2602.17913) — 2-tier architecture:
//!   Tier-2: Immutable raw log (SQLite, append-only)
//!   Tier-1: Provenance index (Vector + Graph, with source pointers)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Source Pointer ─────────────────────────────────────────────────────────

/// A pointer back to the original immutable conversation data.
///
/// Every chunk, entity, relation, and wiki page carries one or more of these.
/// This enables:
///   - Full rebuild: replay the pipeline from the raw log
///   - Auditing: trace any derived fact to its conversational origin
///   - Verification: SHA-256 content hash proves the source hasn't been tampered with
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePointer {
    /// SQLite session ID.
    pub session_id: Uuid,
    /// SQLite message ID.
    pub message_id: Uuid,
    /// SHA-256 hash of the original message content at extraction time.
    /// Used to verify the source hasn't been altered.
    pub content_hash: String,
    /// Optional byte range within the message (for long messages where
    /// only a portion is relevant).
    pub offset_range: Option<(usize, usize)>,
    /// When this pointer was created (i.e., when the projection was built).
    pub derived_at: DateTime<Utc>,
}

impl SourcePointer {
    /// Create a source pointer for a full message.
    pub fn new(session_id: Uuid, message_id: Uuid, content: &str) -> Self {
        Self {
            session_id,
            message_id,
            content_hash: sha256_hash(content),
            offset_range: None,
            derived_at: Utc::now(),
        }
    }

    /// Create a source pointer for a byte range within a message.
    pub fn with_range(
        session_id: Uuid,
        message_id: Uuid,
        content: &str,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            session_id,
            message_id,
            content_hash: sha256_hash(content),
            offset_range: Some((start, end)),
            derived_at: Utc::now(),
        }
    }

    /// Verify that the given content matches the stored hash.
    pub fn verify(&self, content: &str) -> bool {
        sha256_hash(content) == self.content_hash
    }
}

fn sha256_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── Projection Metadata ────────────────────────────────────────────────────

/// Metadata attached to every derived artifact, recording the pipeline
/// version, model, and configuration that produced it.
///
/// When the pipeline or model changes, projections can be invalidated and
/// rebuilt from the raw log by filtering on `pipeline_version` or `model_used`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionMetadata {
    /// Pipeline version that generated this artifact (e.g., "2.0.0").
    pub pipeline_version: String,
    /// LLM model used for extraction (e.g., "deepseek-v4").
    pub model_used: String,
    /// Source pointers back to the raw conversation data.
    pub source_pointers: Vec<SourcePointer>,
    /// Confidence score [0.0, 1.0] assigned by the extraction LLM.
    pub confidence: f32,
    /// When this projection was created.
    pub created_at: DateTime<Utc>,
}

impl ProjectionMetadata {
    pub fn new(
        pipeline_version: impl Into<String>,
        model_used: impl Into<String>,
        source_pointers: Vec<SourcePointer>,
        confidence: f32,
    ) -> Self {
        Self {
            pipeline_version: pipeline_version.into(),
            model_used: model_used.into(),
            source_pointers,
            confidence: confidence.clamp(0.0, 1.0),
            created_at: Utc::now(),
        }
    }

    /// Returns true if this projection is from an older pipeline version.
    pub fn is_stale(&self, current_pipeline_version: &str) -> bool {
        self.pipeline_version != current_pipeline_version
    }

    /// Returns true if this projection used a different model.
    pub fn model_changed(&self, current_model: &str) -> bool {
        self.model_used != current_model
    }
}

// ── Memory Fact ────────────────────────────────────────────────────────────

/// A single unit of long-term memory — what gets stored in MEMORY.md
/// and indexed for retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// Unique slug (kebab-case), used as filename and link target.
    pub name: String,
    /// One-line summary, used in MEMORY.md index and search results.
    pub description: String,
    /// The full fact content (Markdown body).
    pub content: String,
    /// Type classification.
    pub fact_type: FactType,
    /// When this fact was first recorded.
    pub created_at: DateTime<Utc>,
    /// When this fact was last modified.
    pub updated_at: DateTime<Utc>,
    /// Source provenance.
    pub projection: ProjectionMetadata,
    /// Linked memories ([[wikilink]] targets).
    pub links: Vec<String>,
}

/// Classification of a memory fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum FactType {
    /// User persona / preference (e.g., "prefers async/await").
    User,
    /// User feedback on the agent's behavior.
    Feedback,
    /// Project-specific knowledge (e.g., architecture, conventions).
    Project,
    /// External reference (URL, paper, documentation).
    Reference,
}

impl FactType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }
}

// ── Memory Index Entry ────────────────────────────────────────────────────

/// One line in MEMORY.md's index section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexEntry {
    pub name: String,
    pub description: String,
    pub fact_type: FactType,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_pointer_verification() {
        let sp = SourcePointer::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "hello world",
        );
        assert!(sp.verify("hello world"));
        assert!(!sp.verify("hello world!"));
    }

    #[test]
    fn test_source_pointer_hash_stability() {
        let sp1 = SourcePointer::new(
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap(),
            "test content",
        );
        let sp2 = SourcePointer::new(
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap(),
            "test content",
        );
        assert_eq!(sp1.content_hash, sp2.content_hash);
    }

    #[test]
    fn test_projection_metadata_staleness() {
        let meta = ProjectionMetadata::new(
            "1.0.0",
            "deepseek-v4",
            vec![],
            0.9,
        );
        assert!(meta.is_stale("2.0.0"));
        assert!(!meta.is_stale("1.0.0"));
        assert!(meta.model_changed("deepseek-v5"));
    }

    #[test]
    fn test_fact_type_roundtrip() {
        for ty in &[FactType::User, FactType::Feedback, FactType::Project, FactType::Reference] {
            let s = ty.as_str();
            let parsed = FactType::from_str(s).unwrap();
            assert_eq!(parsed, *ty);
        }
    }
}
