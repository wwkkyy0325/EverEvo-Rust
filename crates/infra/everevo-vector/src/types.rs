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
    /// Parse from string with fallback to `Fact`.
    #[allow(clippy::should_implement_trait)]
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

// ── Chunk Constructors ─────────────────────────────────────────────────────

/// Create a `RawChunk` with no source pointers (fresh system-generated data).
pub fn make_chunk(content: String, chunk_type: ChunkType) -> RawChunk {
    RawChunk {
        id: Uuid::new_v4(),
        content,
        source_pointers: vec![],
        projection: ProjectionMetadata::new(env!("CARGO_PKG_VERSION"), "agent", vec![], 0.5),
        chunk_type,
    }
}

/// Create a `RawChunk` with source pointers (traceable to original context).
pub fn make_chunk_with_sources(
    content: String,
    chunk_type: ChunkType,
    sources: Vec<SourcePointer>,
) -> RawChunk {
    RawChunk {
        id: Uuid::new_v4(),
        content,
        source_pointers: sources,
        projection: ProjectionMetadata::new(env!("CARGO_PKG_VERSION"), "agent", vec![], 0.5),
        chunk_type,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_type_as_str() {
        assert_eq!(ChunkType::Preference.as_str(), "preference");
        assert_eq!(ChunkType::Fact.as_str(), "fact");
        assert_eq!(ChunkType::Decision.as_str(), "decision");
        assert_eq!(ChunkType::Task.as_str(), "task");
        assert_eq!(ChunkType::Feedback.as_str(), "feedback");
    }

    #[test]
    fn test_chunk_type_from_str() {
        assert_eq!(ChunkType::from_str("preference"), ChunkType::Preference);
        assert_eq!(ChunkType::from_str("fact"), ChunkType::Fact);
        assert_eq!(ChunkType::from_str("decision"), ChunkType::Decision);
        assert_eq!(ChunkType::from_str("task"), ChunkType::Task);
        assert_eq!(ChunkType::from_str("feedback"), ChunkType::Feedback);
    }

    #[test]
    fn test_chunk_type_from_str_fallback() {
        assert_eq!(ChunkType::from_str("unknown"), ChunkType::Fact);
        assert_eq!(ChunkType::from_str(""), ChunkType::Fact);
    }

    #[test]
    fn test_chunk_type_roundtrip() {
        for ct in &[
            ChunkType::Preference,
            ChunkType::Fact,
            ChunkType::Decision,
            ChunkType::Task,
            ChunkType::Feedback,
        ] {
            assert_eq!(ChunkType::from_str(ct.as_str()), *ct);
        }
    }

    #[test]
    fn test_memory_chunk_construction() {
        let chunk = MemoryChunk {
            id: Uuid::new_v4(),
            content: "test content".into(),
            vector: vec![0.1, 0.2, 0.3],
            source_pointers: vec![],
            projection: everevo_core::memory::ProjectionMetadata::new("1.0", "none", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        };
        assert_eq!(chunk.content, "test content");
        assert_eq!(chunk.vector.len(), 3);
        assert_eq!(chunk.chunk_type, ChunkType::Fact);
        assert_eq!(chunk.retrieval_count, 0);
    }

    #[test]
    fn test_scored_chunk_sort() {
        let chunk = MemoryChunk {
            id: Uuid::new_v4(),
            content: "x".into(),
            vector: vec![1.0],
            source_pointers: vec![],
            projection: everevo_core::memory::ProjectionMetadata::new("1.0", "none", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        };
        let mut scored = vec![
            ScoredChunk {
                chunk: chunk.clone(),
                score: 0.3,
            },
            ScoredChunk {
                chunk: chunk.clone(),
                score: 0.9,
            },
            ScoredChunk {
                chunk: chunk.clone(),
                score: 0.5,
            },
        ];
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(scored[0].score, 0.9);
        assert_eq!(scored[1].score, 0.5);
        assert_eq!(scored[2].score, 0.3);
    }
}
