//! Memory tool — lets the LLM save, search, and delete persistent memories.
//!
//! Actions: add, search, delete

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;

use crate::memory::facts::FactManager;

pub struct MemoryTool {
    manager: Arc<FactManager>,
}

impl MemoryTool {
    pub fn new(manager: Arc<FactManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str { "memory" }

    fn description(&self) -> &str {
        "Manage persistent memories across sessions. \
         Actions: add (save a fact), search (find by keyword), delete (remove outdated). \
         Facts are stored as Markdown files and loaded automatically at session start."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "search", "delete"] },
                "name": { "type": "string", "description": "Kebab-case slug. Required for add/delete." },
                "content": { "type": "string", "description": "Full markdown content. Required for add." },
                "description": { "type": "string", "description": "One-line summary." },
                "fact_type": { "type": "string", "enum": ["user", "feedback", "project", "reference"] },
                "query": { "type": "string", "description": "Keyword search. Used by search." }
            },
            "required": ["action"]
        })
    }

    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        match params["action"].as_str().unwrap_or("") {
            "add" => self.add(&params).await,
            "search" => self.search(&params).await,
            "delete" => self.delete(&params).await,
            other => Err(EverEvoError::InvalidInput(format!("Unknown action: {other}"))),
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
        };

        match self.manager.save(&fact) {
            Ok(()) => Ok(ToolOutput { content: format!("Memory saved: {name}"), is_error: false }),
            Err(e) => Ok(ToolOutput { content: format!("Failed: {e}"), is_error: true }),
        }
    }

    async fn search(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let query = p["query"].as_str().unwrap_or("").to_lowercase();
        let facts = self.manager.load_all().unwrap_or_default();

        let matched: Vec<_> = if query.is_empty() {
            facts.iter().collect()
        } else {
            facts.iter().filter(|f| {
                f.name.to_lowercase().contains(&query)
                    || f.description.to_lowercase().contains(&query)
                    || f.content.to_lowercase().contains(&query)
            }).collect()
        };

        if matched.is_empty() {
            let msg = if query.is_empty() {
                "No memories found.".to_string()
            } else {
                format!("No memories found matching '{query}'.")
            };
            return Ok(ToolOutput { content: msg, is_error: false });
        }

        let lines: Vec<String> = matched.iter().map(|f| {
            format!("- [{name}] {desc} ({ty})",
                name = f.name, desc = f.description, ty = f.fact_type.as_str())
        }).collect();

        Ok(ToolOutput {
            content: format!("{} memories:\n{}", matched.len(), lines.join("\n")),
            is_error: false,
        })
    }

    async fn delete(&self, p: &serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        let name = require_str(p, "name")?;
        match self.manager.delete(name) {
            Ok(()) => Ok(ToolOutput { content: format!("Deleted: {name}"), is_error: false }),
            Err(e) => Ok(ToolOutput { content: format!("Failed: {e}"), is_error: true }),
        }
    }
}

fn require_str<'a>(p: &'a serde_json::Value, key: &str) -> Result<&'a str, EverEvoError> {
    p[key].as_str()
        .ok_or_else(|| EverEvoError::InvalidInput(format!("{key} is required")))
}
