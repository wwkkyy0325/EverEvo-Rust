//! Symbol Registry — auto-populates the Knowledge Graph from runtime registries.
//!
//! Every tool, agent, knowledge source, and constraint becomes an `Entity` in the
//! Knowledge Graph with typed relations. This provides a SPARQL-queryable ontology
//! that higher layers (Meta-Agent, ReviewGate) can consult at runtime.
//!
//! ## Design
//!
//! - **Idempotent registration**: calling `register_tools()` twice produces no duplicates.
//! - **Capability inference**: capabilities are derived from tool names and parameter
//!   schemas using heuristics — no manual annotation needed.
//! - **Opt-out pattern**: if `kg` is `None`, all methods are no-ops. This lets higher
//!   layers compile and test without a running Knowledge Graph.

use std::collections::HashMap;
use std::sync::Arc;

use std::sync::RwLock;

use everevo_core::tool::ToolRegistry;
use everevo_core::EverEvoError;

use super::graph::KnowledgeGraph;
use super::types::{Entity, EntityType, Relation, RelationStatus, SymbolPredicate};

// ── Capability Constants ────────────────────────────────────────────────────

/// Standard capability identifiers (kebab-case, used as entity IDs).
pub const CAP_READ: &str = "can-read";
pub const CAP_WRITE: &str = "can-write";
pub const CAP_EXECUTE: &str = "can-execute";
pub const CAP_SEARCH: &str = "can-search";
pub const CAP_DELEGATE: &str = "can-delegate";
pub const CAP_LEARN: &str = "can-learn";
pub const CAP_FETCH: &str = "can-fetch";

/// All known capabilities (for seeding purposes).
pub const ALL_CAPABILITIES: &[&str] = &[
    CAP_READ, CAP_WRITE, CAP_EXECUTE, CAP_SEARCH, CAP_DELEGATE, CAP_LEARN, CAP_FETCH,
];

// ── Symbol Registry ─────────────────────────────────────────────────────────

/// Registry that populates the Knowledge Graph with symbol entities.
///
/// Wraps a `KnowledgeGraph` behind `Arc<RwLock<>>` for shared access across threads.
pub struct SymbolRegistry {
    kg: Option<Arc<RwLock<KnowledgeGraph>>>,
}

impl SymbolRegistry {
    /// Create a new registry. Pass `None` to disable (no-op mode for testing).
    pub fn new(kg: Option<Arc<RwLock<KnowledgeGraph>>>) -> Self {
        Self { kg }
    }

    /// Whether the registry has a backing knowledge graph.
    pub fn is_active(&self) -> bool {
        self.kg.is_some()
    }

    // ── Tool Registration ──────────────────────────────────────────────

    /// Scan a `ToolRegistry` and create an `Entity` + `Capability` relations
    /// for each tool. Idempotent — re-running does not create duplicates.
    ///
    /// Returns the number of tool entities registered.
    pub fn register_tools(&self, registry: &ToolRegistry) -> Result<usize, EverEvoError> {
        let kg = match &self.kg {
            Some(kg) => kg,
            None => return Ok(0),
        };
        let mut kg = kg.write().unwrap_or_else(|e| e.into_inner());

        // Seed capability entities (idempotent — upsert skips existing).
        let now = chrono::Utc::now();
        for cap_id in ALL_CAPABILITIES {
            kg.upsert_entity(Entity {
                id: cap_id.to_string(),
                label: cap_label(cap_id),
                entity_type: EntityType::Capability,
                properties: HashMap::new(),
                sources: Vec::new(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });
        }

        let mut count = 0usize;
        for name in registry.names() {
            let tool = match registry.get(name) {
                Some(t) => t,
                None => continue,
            };
            let entity_id = format!("tool-{name}");

            // Build properties
            let mut props = HashMap::new();
            props.insert("tool_name".into(), name.to_string());

            let desc = tool.description();
            // Take first sentence for short label
            let label = desc
                .split('.')
                .next()
                .unwrap_or(name)
                .trim()
                .to_string();
            props.insert("description".into(), desc.to_string());

            let risk = format!("{:?}", tool.risk_level());
            props.insert("risk_level".into(), risk);

            kg.upsert_entity(Entity {
                id: entity_id.clone(),
                label,
                entity_type: EntityType::Tool,
                properties: props,
                sources: Vec::new(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });

            // Infer capabilities from tool name and schema
            let schema = tool.parameters_schema();
            let capabilities = infer_capabilities(name, &schema);

            for cap_id in &capabilities {
                kg.add_relation_many(Relation {
                    from: entity_id.clone(),
                    predicate: SymbolPredicate::HasCapability.as_uri_fragment(),
                    to: cap_id.clone(),
                    status: RelationStatus::Active,
                    valid_from: now,
                    valid_until: None,
                    sources: Vec::new(),
                });
            }

            // Also add ConstrainedBy relation for high-risk or critical tools
            let risk = tool.risk_level();
            if matches!(risk, everevo_core::types::RiskLevel::High) {
                kg.add_relation_many(Relation {
                    from: entity_id.clone(),
                    predicate: SymbolPredicate::ConstrainedBy.as_uri_fragment(),
                    to: "constraint-permission".into(),
                    status: RelationStatus::Active,
                    valid_from: now,
                    valid_until: None,
                    sources: Vec::new(),
                });
            }

            count += 1;
        }

        // Seed the permission constraint entity
        kg.upsert_entity(Entity {
            id: "constraint-permission".into(),
            label: "Permission Level Constraint".into(),
            entity_type: EntityType::Constraint,
            properties: {
                let mut p = HashMap::new();
                p.insert("key".into(), "permission_level".into());
                p.insert("applies_to".into(), "tools with risk_level >= High".into());
                p
            },
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
            merged_into: None,
        });

        tracing::info!(count, "Symbol registry: registered tool entities in knowledge graph");
        Ok(count)
    }

    /// Register the main agent entity with its tool set.
    pub fn register_agent(
        &self,
        agent_id: &str,
        tool_names: &[&str],
    ) -> Result<(), EverEvoError> {
        let kg = match &self.kg {
            Some(kg) => kg,
            None => return Ok(()),
        };
        let mut kg = kg.write().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Utc::now();

        let entity_id = format!("agent-{agent_id}");
        kg.upsert_entity(Entity {
            id: entity_id.clone(),
            label: format!("Agent: {agent_id}"),
            entity_type: EntityType::Other("Agent".into()),
            properties: {
                let mut p = HashMap::new();
                p.insert("tools".into(), tool_names.join(", "));
                p
            },
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
            merged_into: None,
        });

        // Connect to available tools
        for tool_name in tool_names {
            kg.add_relation_many(Relation {
                from: entity_id.clone(),
                predicate: SymbolPredicate::DependsOn.as_uri_fragment(),
                to: format!("tool-{tool_name}"),
                status: RelationStatus::Active,
                valid_from: now,
                valid_until: None,
                sources: Vec::new(),
            });
        }

        Ok(())
    }

    // ── SPARQL Queries ─────────────────────────────────────────────────

    /// Find all tool entities that have a specific capability.
    ///
    /// Uses the HashMap index (fast path) rather than SPARQL for hot-path queries.
    pub fn find_by_capability(&self, capability: &str) -> Vec<Entity> {
        let kg = match &self.kg {
            Some(kg) => kg,
            None => return Vec::new(),
        };
        let kg = kg.read().unwrap_or_else(|e| e.into_inner());

        // Find relations with predicate=hasCapability, to=capability
        let pred = SymbolPredicate::HasCapability.as_uri_fragment();
        let from_ids: Vec<String> = kg
            .find_relations_by_predicate_any(&pred)
            .iter()
            .filter(|r| r.to == capability)
            .map(|r| r.from.clone())
            .collect();

        from_ids
            .iter()
            .filter_map(|id| kg.get_entity(id))
            .collect()
    }

    /// Return all constraint entities that apply to a given entity.
    pub fn constraints_on(&self, entity_id: &str) -> Vec<Entity> {
        let kg = match &self.kg {
            Some(kg) => kg,
            None => return Vec::new(),
        };
        let kg = kg.read().unwrap_or_else(|e| e.into_inner());

        let pred = SymbolPredicate::ConstrainedBy.as_uri_fragment();
        let constraint_ids: Vec<String> = kg
            .find_relations_by_predicate(entity_id, &pred)
            .iter()
            .map(|r| r.to.clone())
            .collect();

        constraint_ids
            .iter()
            .filter_map(|id| kg.get_entity(id))
            .collect()
    }

    /// Suggest a minimal set of tool entities whose capabilities collectively
    /// cover all of the `required` capabilities.
    ///
    /// Uses a greedy set-cover heuristic on the entity-capability graph.
    /// Returns ranked suggestions (best first), each as a list of entity IDs.
    ///
    /// This is the core "entity behavior composition" query — given a goal
    /// expressed as required capabilities, find the tools that achieve it.
    pub fn suggest_composition(&self, required: &[&str]) -> Vec<Vec<String>> {
        let kg = match &self.kg {
            Some(kg) => kg,
            None => return Vec::new(),
        };
        let kg = kg.read().unwrap_or_else(|e| e.into_inner());
        let pred = SymbolPredicate::HasCapability.as_uri_fragment();

        // Build capability→tools index
        let mut cap_to_tools: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for rel in kg.find_relations_by_predicate_any(&pred) {
            cap_to_tools
                .entry(rel.to.clone())
                .or_default()
                .push(rel.from.clone());
        }

        // Greedy set cover: repeatedly pick the tool covering the most uncovered caps
        let mut uncovered: std::collections::HashSet<String> = required
            .iter()
            .map(|c| c.to_string())
            .collect();
        let mut selected: Vec<String> = Vec::new();
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();

        while !uncovered.is_empty() {
            let mut best_tool: Option<String> = None;
            let mut best_count = 0usize;
            for (cap, tools) in &cap_to_tools {
                if !uncovered.contains(cap) {
                    continue;
                }
                for tool_id in tools {
                    if used.contains(tool_id) {
                        continue;
                    }
                    // Count how many UNCOVERED capabilities this tool would cover
                    let count = cap_to_tools
                        .iter()
                        .filter(|(c, ts)| uncovered.contains(*c) && ts.contains(tool_id))
                        .count();
                    if count > best_count {
                        best_count = count;
                        best_tool = Some(tool_id.clone());
                    }
                }
            }
            match best_tool {
                Some(tool_id) => {
                    // Remove covered capabilities
                    let caps_covered: Vec<String> = cap_to_tools
                        .iter()
                        .filter(|(_, ts)| ts.contains(&tool_id))
                        .map(|(c, _)| c.clone())
                        .collect();
                    for c in &caps_covered {
                        uncovered.remove(c);
                    }
                    used.insert(tool_id.clone());
                    selected.push(tool_id);
                }
                None => break, // cannot cover remaining capabilities
            }
        }

        if selected.is_empty() {
            Vec::new()
        } else {
            vec![selected]
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Infer capabilities from a tool's name and JSON parameter schema.
fn infer_capabilities(name: &str, schema: &serde_json::Value) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();
    let name_lower = name.to_lowercase();

    // Name-based heuristics
    if name_lower.contains("read")
        || name_lower.contains("list")
        || name_lower.contains("code_map")
        || name_lower.contains("search")
    {
        caps.push(CAP_READ.into());
    }
    if name_lower.contains("write") || name_lower.contains("save") {
        caps.push(CAP_WRITE.into());
    }
    if name_lower.contains("shell") || name_lower.contains("execute") {
        caps.push(CAP_EXECUTE.into());
    }
    if name_lower.contains("search") || name_lower.contains("find") || name_lower.contains("query")
    {
        caps.push(CAP_SEARCH.into());
    }
    if name_lower.contains("task") || name_lower.contains("team") || name_lower.contains("cluster")
    {
        caps.push(CAP_DELEGATE.into());
    }
    if name_lower.contains("memory") || name_lower.contains("extract") {
        caps.push(CAP_LEARN.into());
    }
    if name_lower.contains("fetch") || name_lower.contains("download") || name_lower.contains("web")
    {
        caps.push(CAP_FETCH.into());
    }

    // Schema-based heuristics: look at parameter property names
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let prop_names: Vec<&str> = props.keys().map(|s| s.as_str()).collect();
        for pn in &prop_names {
            let pl = pn.to_lowercase();
            if (pl.contains("file_path") || pl.contains("path"))
                && !caps.contains(&CAP_READ.to_string())
            {
                caps.push(CAP_READ.into());
            }
            if (pl.contains("command") || pl.contains("cmd"))
                && !caps.contains(&CAP_EXECUTE.to_string())
            {
                caps.push(CAP_EXECUTE.into());
            }
            if (pl.contains("query") || pl.contains("pattern"))
                && !caps.contains(&CAP_SEARCH.to_string())
            {
                caps.push(CAP_SEARCH.into());
            }
            if pl.contains("url") && !caps.contains(&CAP_FETCH.to_string()) {
                caps.push(CAP_FETCH.into());
            }
        }
    }

    if caps.is_empty() {
        caps.push("can-read".into()); // conservative default
    }

    caps
}

/// Human-readable label for a capability ID.
fn cap_label(cap_id: &str) -> String {
    match cap_id {
        CAP_READ => "Can Read".into(),
        CAP_WRITE => "Can Write".into(),
        CAP_EXECUTE => "Can Execute".into(),
        CAP_SEARCH => "Can Search".into(),
        CAP_DELEGATE => "Can Delegate".into(),
        CAP_LEARN => "Can Learn".into(),
        CAP_FETCH => "Can Fetch".into(),
        other => other.to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_core::tool::{Tool, ToolOutput, ToolRegistry};
    use everevo_core::types::RiskLevel;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    // Minimal test tool — implements Tool for SymbolRegistry testing
    struct TestTool {
        name: &'static str,
        desc: &'static str,
        schema: serde_json::Value,
        risk: RiskLevel,
    }

    #[async_trait::async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters_schema(&self) -> serde_json::Value {
            self.schema.clone()
        }
        fn risk_level(&self) -> RiskLevel {
            self.risk
        }
        async fn execute(
            &self,
            _params: serde_json::Value,
            _cancel: Option<&CancellationToken>,
        ) -> Result<ToolOutput, everevo_core::EverEvoError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    fn test_registry() -> ToolRegistry {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TestTool {
            name: "read_file",
            desc: "Read a file from disk.",
            schema: serde_json::json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
            risk: RiskLevel::Low,
        }));
        reg.register(Arc::new(TestTool {
            name: "shell",
            desc: "Execute a shell command.",
            schema: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            risk: RiskLevel::High,
        }));
        reg.register(Arc::new(TestTool {
            name: "web_search",
            desc: "Search the web.",
            schema: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            risk: RiskLevel::Low,
        }));
        reg.register(Arc::new(TestTool {
            name: "task",
            desc: "Spawn a sub-agent.",
            schema: serde_json::json!({"type": "object", "properties": {"description": {"type": "string"}}}),
            risk: RiskLevel::Medium,
        }));
        reg
    }

    #[test]
    fn test_register_tools_populates_entities() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));

        let count = sr.register_tools(&reg).unwrap();
        assert_eq!(count, 4, "Should register 4 tool entities");

        let kg = kg.read().unwrap_or_else(|e| e.into_inner());
        assert!(kg.get_entity("tool-read_file").is_some());
        assert!(kg.get_entity("tool-shell").is_some());
        assert!(kg.get_entity("tool-web_search").is_some());
        assert!(kg.get_entity("tool-task").is_some());
        assert!(kg.get_entity(CAP_READ).is_some());
        assert!(kg.get_entity(CAP_EXECUTE).is_some());
        assert!(kg.get_entity(CAP_SEARCH).is_some());
        assert!(kg.get_entity(CAP_DELEGATE).is_some());
    }

    #[test]
    fn test_register_tools_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));

        let c1 = sr.register_tools(&reg).unwrap();
        let c2 = sr.register_tools(&reg).unwrap();
        assert_eq!(c1, c2);
        let kg = kg.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(kg.find_by_type(&EntityType::Tool).len(), 4);
    }

    #[test]
    fn test_find_by_capability() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));
        sr.register_tools(&reg).unwrap();

        let executors = sr.find_by_capability(CAP_EXECUTE);
        assert_eq!(executors.len(), 1);
        assert!(executors[0].id.contains("shell"));

        let readers = sr.find_by_capability(CAP_READ);
        assert!(readers.iter().any(|e| e.id.contains("read_file")));
    }

    #[test]
    fn test_constraints_on() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));
        sr.register_tools(&reg).unwrap();

        let constraints = sr.constraints_on("tool-shell");
        assert!(!constraints.is_empty());
        assert!(constraints.iter().any(|c| c.id == "constraint-permission"));
    }

    #[test]
    fn test_register_agent() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));
        sr.register_tools(&reg).unwrap();
        sr.register_agent("main", &["read_file", "shell"]).unwrap();

        let kg = kg.read().unwrap_or_else(|e| e.into_inner());
        assert!(kg.get_entity("agent-main").is_some());
        let pred = SymbolPredicate::DependsOn.as_uri_fragment();
        let deps = kg.find_relations_by_predicate("agent-main", &pred);
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_noop_mode_without_kg() {
        let reg = test_registry();
        let sr = SymbolRegistry::new(None);
        assert!(!sr.is_active());
        assert_eq!(sr.register_tools(&reg).unwrap(), 0);
        assert!(sr.find_by_capability(CAP_EXECUTE).is_empty());
    }

    #[test]
    fn test_suggest_composition_covers_capabilities() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));
        sr.register_tools(&reg).unwrap();

        // Verify data is there first
        let readers = sr.find_by_capability(CAP_READ);
        let executors = sr.find_by_capability(CAP_EXECUTE);
        assert!(!readers.is_empty(), "should have at least one reader");
        assert!(!executors.is_empty(), "should have at least one executor");

        // Request capabilities: read + execute
        let compositions = sr.suggest_composition(&[CAP_READ, CAP_EXECUTE]);
        assert!(!compositions.is_empty(), "Should find a composition covering read+execute");
        let tool_ids = &compositions[0];
        // Collect covered capabilities from selected tools
        let mut covered = std::collections::HashSet::new();
        for tid in tool_ids {
            // Each tool covers all capabilities it's related to via HasCapability
            let kg = kg.read().unwrap_or_else(|e| e.into_inner());
            let pred = SymbolPredicate::HasCapability.as_uri_fragment();
            for rel in kg.find_relations_by_predicate(tid, &pred) {
                covered.insert(rel.to.clone());
            }
        }
        assert!(covered.contains(CAP_READ), "selected tools should cover can-read");
        assert!(covered.contains(CAP_EXECUTE), "selected tools should cover can-execute");
    }

    #[test]
    fn test_suggest_composition_empty_for_unknown_capability() {
        let dir = TempDir::new().unwrap();
        let kg = Arc::new(RwLock::new(KnowledgeGraph::open(dir.path()).unwrap()));
        let reg = test_registry();
        let sr = SymbolRegistry::new(Some(kg.clone()));
        sr.register_tools(&reg).unwrap();

        let compositions = sr.suggest_composition(&["can-teleport"]);
        assert!(compositions.is_empty());
    }
}
