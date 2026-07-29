//! Document types — domain document, chunks, and metadata.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A document stored in a domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainDocument {
    pub id: Uuid,
    pub domain_id: String,
    /// Original filename.
    pub filename: String,
    /// SHA-256 hash of raw content (for dedup).
    pub content_hash: String,
    /// Full extracted text.
    pub content: String,
    /// File type.
    pub file_type: String,
    /// Number of chunks.
    pub chunk_count: usize,
    /// How this document was assigned to its domain.
    pub source: DocumentSource,
    /// When this was first ingested.
    pub created_at: DateTime<Utc>,
    /// Superseded by a newer version (if updated).
    pub superseded_by: Option<Uuid>,
}

/// How a document was assigned to a domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DocumentSource {
    /// User explicitly chose this domain — locked, never auto-moved.
    Manual,
    /// Auto-classified from the global inbox.
    AutoClassified,
    /// Imported from a project directory (grouped with sibling files).
    ProjectGroup,
}

/// A chunk of a document, ready for vector indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainChunk {
    pub id: Uuid,
    pub document_id: Uuid,
    pub domain_id: String,
    pub content: String,
    /// Position in the original document.
    pub chunk_index: usize,
    /// 384-dim embedding.
    pub vector: Vec<f32>,
    /// Type of content (text, code, table, heading).
    pub chunk_type: ChunkType,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChunkType {
    Text,
    Code,
    Table,
    Heading,
}

/// Lightweight document metadata (for listing).
#[derive(Debug, Clone, Serialize)]
pub struct DocumentMeta {
    pub filename: String,
    pub size_bytes: u64,
    pub modified: DateTime<Utc>,
}
