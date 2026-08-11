//! Memory tool — lets the LLM save, search, delete, and query the knowledge graph.
//!
//! Actions: add, search, delete, kg_search

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::memory::facts::{fact_visible_to, FactManager};
use everevo_knowledge::graph::KnowledgeGraph;

pub struct MemoryTool {
    manager: Arc<FactManager>,
    db: Option<everevo_db::Database>,
    kg: Option<Arc<std::sync::RwLock<KnowledgeGraph>>>,
    /// Originating session. New facts are tagged with it (session-scoped
    /// working memory, 分层记忆) unless the caller requests `scope: "global"`.
    session_id: Option<Uuid>,
}

impl MemoryTool {
    pub fn new(manager: Arc<FactManager>) -> Self {
        Self {
            manager,
            db: None,
            kg: None,
            session_id: None,
        }
    }
    pub fn with_db(mut self, db: everevo_db::Database) -> Self {
        self.db = Some(db);
        self
    }
    pub fn with_kg(mut self, kg: Arc<std::sync::RwLock<KnowledgeGraph>>) -> Self {
        self.kg = Some(kg);
        self
    }
    pub fn with_session_id(mut self, session_id: Option<Uuid>) -> Self {
        self.session_id = session_id;
        self
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Manage layered memories (分层记忆). \
         Actions: add (save a fact), search (find by keyword), delete (remove outdated). \
         Facts are stored as Markdown files and loaded automatically at session start. \
         Two tiers: by default a saved fact is SESSION-SCOPED working memory (visible only to \
         this session — strictly isolated). Pass scope=\"global\" to promote it to cross-session \
         long-term memory (visible to all sessions, injected on demand)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "search", "delete", "kg_search"] },
                "name": { "type": "string", "description": "Kebab-case slug. Required for add/delete." },
                "content": { "type": "string", "description": "Full markdown content. Required for add." },
                "description": { "type": "string", "description": "One-line summary." },
                "fact_type": { "type": "string", "enum": ["user", "feedback", "project", "reference"] },
                "scope": { "type": "string", "enum": ["session", "global"], "description": "Memory tier: 'session' (default) = this session only; 'global' = cross-session long-term memory." },
                "query": { "type": "string", "description": "Keyword search. Used by search." }
            },
            "required": ["action"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        match params["action"].as_str().unwrap_or("") {
            "add" => self.add(&params).await,
            "search" => self.search(&params).await,
            "delete" => self.delete(&params).await,
            "kg_search" => self.kg_search(&params).await,
            other => Err(EverEvoError::InvalidInput(format!(
                "Unknown action: {other}"
            ))),
        }
    }
}

impl MemoryTool {
    async fn add(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let name = require_str(p, "name")?;
        let content = require_str(p, "content")?;
        let desc = p["description"].as_str().unwrap_or("").to_string();
        let ft = FactType::from_str(p["fact_type"].as_str().unwrap_or("project"))
            .unwrap_or(FactType::Project);

        // Memory tier (分层记忆): default session-scoped working memory;
        // `scope: "global"` promotes to cross-session long-term memory.
        let session = match p["scope"].as_str().unwrap_or("session") {
            "global" => Some("global".into()),
            _ => self.session_id.map(|sid| sid.to_string()),
        };

        let now = chrono::Utc::now();
        let fact = MemoryFact {
            name: name.to_string(),
            description: desc,
            content: content.to_string(),
            fact_type: ft,
            created_at: now,
            updated_at: now,
            projection: ProjectionMetadata::new("2.0.0", "llm-extracted", vec![], 0.85),
            links: Vec::new(),
            session,
        };

        match self.manager.save_async(fact.clone()).await {
            Ok(()) => Ok(ToolOutput {
                content: format!("Memory saved: {name}"),
                is_error: false,
                ..Default::default()
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed: {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }

    async fn search(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let query = p["query"].as_str().unwrap_or("").to_lowercase();

        // Use SQLite FTS5 indexed search when available (O(log n)).
        // The FTS table has no session column — filter hits to facts visible
        // to this session (分层记忆 strict isolation).
        if let Some(ref db) = self.db {
            match db.search_facts(&query, 20).await {
                Ok(rows) if !rows.is_empty() => {
                    let visible: Vec<_> = rows
                        .into_iter()
                        .filter(|r| {
                            self.manager
                                .load(&r.id)
                                .ok()
                                .flatten()
                                .map(|f| fact_visible_to(&f, self.session_id.as_ref()))
                                .unwrap_or(true)
                        })
                        .collect();
                    if !visible.is_empty() {
                        let lines: Vec<String> = visible
                            .iter()
                            .map(|r| format!("- [{}] {} ({})", r.id, r.description, r.fact_type))
                            .collect();
                        return Ok(ToolOutput {
                            content: format!("{} memories:\n{}", visible.len(), lines.join("\n")),
                            is_error: false,
                            ..Default::default()
                        });
                    }
                }
                Ok(_) => { /* fall through to linear scan */ }
                Err(e) => {
                    tracing::warn!(error = %e, "FTS5 search failed — falling back to linear scan")
                }
            }
        }

        // Linear scan fallback (O(n), file-based) — session-filtered.
        let facts: Vec<_> = self
            .manager
            .load_all()
            .unwrap_or_default()
            .into_iter()
            .filter(|f| fact_visible_to(f, self.session_id.as_ref()))
            .collect();
        let matched: Vec<_> = if query.is_empty() {
            facts.iter().collect()
        } else {
            facts
                .iter()
                .filter(|f| {
                    f.name.to_lowercase().contains(&query)
                        || f.description.to_lowercase().contains(&query)
                        || f.content.to_lowercase().contains(&query)
                })
                .collect()
        };

        if matched.is_empty() {
            return Ok(ToolOutput {
                content: if query.is_empty() {
                    "No memories found.".into()
                } else {
                    format!("No memories matching '{query}'.")
                },
                is_error: false,
                ..Default::default()
            });
        }
        let lines: Vec<String> = matched
            .iter()
            .map(|f| {
                format!(
                    "- [{}] {} ({})",
                    f.name,
                    f.description,
                    f.fact_type.as_str()
                )
            })
            .collect();
        Ok(ToolOutput {
            content: format!("{} memories:\n{}", matched.len(), lines.join("\n")),
            is_error: false,
            ..Default::default()
        })
    }

    async fn delete(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let name = require_str(p, "name")?;
        match self.manager.delete(name) {
            Ok(()) => Ok(ToolOutput {
                content: format!("Deleted: {name}"),
                is_error: false,
                ..Default::default()
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed: {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }

    async fn kg_search(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let query = p["query"].as_str().unwrap_or("");
        if query.is_empty() {
            return Ok(ToolOutput {
                content: "query is required for kg_search".into(),
                is_error: true,
                ..Default::default()
            });
        }
        let kg = self
            .kg
            .as_ref()
            .ok_or_else(|| EverEvoError::Internal("Knowledge graph not available".into()))?;
        let kg = kg.read().unwrap_or_else(|e| e.into_inner());

        let entities = kg.search(query);
        if entities.is_empty() {
            return Ok(ToolOutput {
                content: format!("No entities found matching '{query}' in knowledge graph."),
                is_error: false,
                ..Default::default()
            });
        }

        let mut lines: Vec<String> = Vec::new();
        for entity in entities.iter().take(8) {
            let outgoing = kg.outgoing(&entity.id);
            if outgoing.is_empty() {
                lines.push(format!(
                    "- `{}` ({})",
                    entity.label,
                    entity.entity_type.as_str()
                ));
            } else {
                for rel in outgoing.iter().take(3) {
                    lines.push(format!(
                        "- `{}` ({}) → {} → `{}`",
                        entity.label,
                        entity.entity_type.as_str(),
                        rel.predicate,
                        rel.to
                    ));
                }
            }
        }
        lines.dedup();

        let content = format!(
            "{} entities for '{}':\n{}\n\nUse `memory` with `action: \"kg_search\"` and a different query to explore more.",
            entities.len().min(8),
            query,
            lines.join("\n")
        );
        Ok(ToolOutput {
            content,
            is_error: false,
            ..Default::default()
        })
    }
}

fn require_str<'a>(p: &'a serde_json::Value, key: &str) -> Result<&'a str, EverEvoError> {
    p[key]
        .as_str()
        .ok_or_else(|| EverEvoError::InvalidInput(format!("{key} is required")))
}
