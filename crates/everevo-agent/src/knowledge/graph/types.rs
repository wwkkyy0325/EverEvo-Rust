//! Knowledge graph types — entities, relations, and RDF triples.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use everevo_core::memory::SourcePointer;

// ── Triple ──────────────────────────────────────────────────────────────────

/// A single RDF-like triple: (subject, predicate, object).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

// ── Entity ──────────────────────────────────────────────────────────────────

/// An entity node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier (URI or slug).
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Type classification.
    pub entity_type: EntityType,
    /// Key-value properties.
    pub properties: HashMap<String, String>,
    /// Source pointers to raw conversation data.
    pub sources: Vec<SourcePointer>,
    /// When this entity was first created.
    pub created_at: DateTime<Utc>,
    /// When this entity was last modified.
    pub updated_at: DateTime<Utc>,
    /// If this entity was merged into another, the canonical entity's ID.
    /// Merged entities are NOT deleted — they are preserved with this pointer.
    pub merged_into: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityType {
    Person,
    Project,
    Tool,
    Concept,
    File,
    Event,
    Other(String),
}

impl EntityType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Person => "Person",
            Self::Project => "Project",
            Self::Tool => "Tool",
            Self::Concept => "Concept",
            Self::File => "File",
            Self::Event => "Event",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Relation ────────────────────────────────────────────────────────────────

/// A labeled relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Source entity ID.
    pub from: String,
    /// Relationship predicate.
    pub predicate: String,
    /// Target entity ID.
    pub to: String,
    /// Status: active, superseded, or contradicted.
    pub status: RelationStatus,
    /// When this relation became valid.
    pub valid_from: DateTime<Utc>,
    /// When this relation was superseded (if applicable).
    pub valid_until: Option<DateTime<Utc>>,
    /// Source pointers.
    pub sources: Vec<SourcePointer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationStatus {
    Active,
    Superseded,
    Contradicted,
}
