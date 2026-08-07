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
    /// An agent capability (e.g. can_read, can_execute, can_delegate).
    Capability,
    /// A knowledge source (e.g. memory store, RAG pipeline, domain doc).
    KnowledgeSource,
    /// A runtime constraint (e.g. max_tokens, permission_level, sandbox_tier).
    Constraint,
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
            Self::Capability => "Capability",
            Self::KnowledgeSource => "KnowledgeSource",
            Self::Constraint => "Constraint",
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

// ── Symbol Predicate ─────────────────────────────────────────────────────

/// Standardized relation predicates for the symbol ontology.
///
/// Used by `SymbolRegistry` when connecting entities in the knowledge graph.
/// Each variant maps to a URI under `http://everevo.io/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolPredicate {
    /// Entity A depends on entity B for its operation.
    DependsOn,
    /// Entity A has a capability B.
    HasCapability,
    /// Entity A implements interface/behaviour B.
    Implements,
    /// Entity A is a specialization of entity B.
    Specializes,
    /// Entity A conflicts with entity B (cannot coexist).
    ConflictsWith,
    /// Entity A requires the presence of entity B.
    Requires,
    /// Entity A produces entity B as output.
    Produces,
    /// Entity A is constrained by constraint entity B.
    ConstrainedBy,
    /// Escape hatch for custom predicates not in the standard set.
    Custom(String),
}

impl SymbolPredicate {
    /// Return the URI fragment for this predicate (used in SPARQL).
    pub fn as_uri_fragment(&self) -> String {
        match self {
            Self::DependsOn => "dependsOn".into(),
            Self::HasCapability => "hasCapability".into(),
            Self::Implements => "implements".into(),
            Self::Specializes => "specializes".into(),
            Self::ConflictsWith => "conflictsWith".into(),
            Self::Requires => "requires".into(),
            Self::Produces => "produces".into(),
            Self::ConstrainedBy => "constrainedBy".into(),
            Self::Custom(s) => s.clone(),
        }
    }

    /// Create from a URI fragment string.
    pub fn from_uri_fragment(s: &str) -> Self {
        match s {
            "dependsOn" => Self::DependsOn,
            "hasCapability" => Self::HasCapability,
            "implements" => Self::Implements,
            "specializes" => Self::Specializes,
            "conflictsWith" => Self::ConflictsWith,
            "requires" => Self::Requires,
            "produces" => Self::Produces,
            "constrainedBy" => Self::ConstrainedBy,
            other => Self::Custom(other.into()),
        }
    }
}

impl std::fmt::Display for SymbolPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_uri_fragment())
    }
}
