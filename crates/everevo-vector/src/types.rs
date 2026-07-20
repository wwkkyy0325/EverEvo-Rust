//! Vector store types — memory chunks, chunk types, and search results.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use everevo_core::memory::{ProjectionMetadata, SourcePointer};

/// A single unit of vector-indexed memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    pub id: Uuid,
    pub content: String,
    pub vector: Vec<f32>,
    pub source_pointers: Vec<SourcePointer>,
    pub projection: ProjectionMetadata,
    pub chunk_type: ChunkType,
    pub created_at: DateTime<Utc>,
    pub retrieval_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChunkType {
    Preference,
    Fact,
    Decision,
    Task,
    Feedback,
}

impl ChunkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Task => "task",
            Self::Feedback => "feedback",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "preference" => Self::Preference,
            "fact" => Self::Fact,
            "decision" => Self::Decision,
            "task" => Self::Task,
            "feedback" => Self::Feedback,
            _ => Self::Fact,
        }
    }
}

/// A search result with relevance score.
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: MemoryChunk,
    pub score: f32,
}

/// A chunk before embedding — used as input to `insert_texts`.
pub struct RawChunk {
    pub id: Uuid,
    pub content: String,
    pub source_pointers: Vec<SourcePointer>,
    pub projection: ProjectionMetadata,
    pub chunk_type: ChunkType,
}
