// ── In-memory primary storage (HashMap entities + Vec relations) ────────────

use super::KnowledgeGraph;
use super::super::resolver::{EntityResolver, ResolveStats};
use super::super::types::{Entity, EntityType, Relation, RelationStatus};

impl KnowledgeGraph {
    // ── Entity CRUD (HashMap primary) ─────────────────────────────────
    pub fn upsert_entity(&mut self, entity: Entity) -> bool {
        let is_new = !self.entities.contains_key(&entity.id);
        self.entities.insert(entity.id.clone(), entity);
        is_new
    }
    pub fn get_entity(&self, id: &str) -> Option<Entity> {
        self.entities.get(id).cloned()
    }
    pub fn find_by_type(&self, entity_type: &EntityType) -> Vec<Entity> {
        self.entities
            .values()
            .filter(|e| &e.entity_type == entity_type)
            .cloned()
            .collect()
    }
    pub fn search(&self, query: &str) -> Vec<Entity> {
        let q = query.to_lowercase();
        self.entities
            .values()
            .filter(|e| e.label.to_lowercase().contains(&q) || e.id.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }
    pub fn all_entities(&self) -> Vec<Entity> {
        self.entities.values().cloned().collect()
    }
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }
    pub fn active_entity_count(&self) -> usize {
        self.entities
            .values()
            .filter(|e| e.merged_into.is_none())
            .count()
    }

    // ── Relation CRUD (Vec primary) ───────────────────────────────────
    pub fn add_relation(&mut self, relation: Relation) {
        for existing in self.relations.iter_mut() {
            if existing.from == relation.from
                && existing.predicate == relation.predicate
                && existing.to != relation.to
                && existing.status == RelationStatus::Active
            {
                existing.status = RelationStatus::Superseded;
                existing.valid_until = Some(relation.valid_from);
            }
        }
        self.relations.push(Relation {
            status: RelationStatus::Active,
            ..relation
        });
    }
    pub fn outgoing(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.from == entity_id && r.status == RelationStatus::Active)
            .collect()
    }
    pub fn incoming(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.to == entity_id && r.status == RelationStatus::Active)
            .collect()
    }
    pub fn relation_count(&self) -> usize {
        self.relations
            .iter()
            .filter(|r| r.status == RelationStatus::Active)
            .count()
    }

    /// Add a many-to-many relation (no supersede semantics).
    ///
    /// Unlike `add_relation()`, this does NOT supersede existing relations
    /// with the same `(from, predicate)` pair. Use for `DependsOn`,
    /// `HasCapability`, and other one-to-many predicates.
    pub fn add_relation_many(&mut self, relation: Relation) {
        // Only skip if the EXACT same relation already exists
        let already_exists = self.relations.iter().any(|r| {
            r.from == relation.from
                && r.predicate == relation.predicate
                && r.to == relation.to
                && r.status == RelationStatus::Active
        });
        if !already_exists {
            self.relations.push(Relation {
                status: RelationStatus::Active,
                ..relation
            });
        }
    }

    /// Find active relations matching a given predicate.
    ///
    /// Matches on the predicate string directly (both raw strings and
    /// `SymbolPredicate::as_uri_fragment()` values).
    pub fn find_relations_by_predicate(&self, from: &str, predicate: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| {
                r.from == from && r.predicate == predicate && r.status == RelationStatus::Active
            })
            .collect()
    }

    /// Find all active relations where the predicate matches (any from/to).
    pub fn find_relations_by_predicate_any(&self, predicate: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.predicate == predicate && r.status == RelationStatus::Active)
            .collect()
    }

    /// Seed the knowledge graph with project structure entities so
    /// `memory kg_search` returns useful results immediately — no empty
    /// "no entities" response on first use.
    ///
    /// Only seeds if the graph is empty (idempotent). Call after `open()`.
    pub fn seed_project_structure(&mut self, crate_names: &[&str]) {
        if self.entity_count() > 0 {
            return; // already seeded
        }
        let now = chrono::Utc::now();
        let empty_props = std::collections::HashMap::new();
        let empty_sources = Vec::new();

        // Root project entity
        self.upsert_entity(Entity {
            id: "everevo".into(),
            label: "EverEvo".into(),
            entity_type: EntityType::Project,
            properties: empty_props.clone(),
            sources: empty_sources.clone(),
            created_at: now,
            updated_at: now,
            merged_into: None,
        });

        // Crate entities with relations
        for &name in crate_names {
            let id = format!("crate-{name}");
            self.upsert_entity(Entity {
                id: id.clone(),
                label: format!("Crate: {name}"),
                entity_type: EntityType::Other("Crate".into()),
                properties: empty_props.clone(),
                sources: empty_sources.clone(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });
            self.add_relation(Relation {
                from: "everevo".into(),
                predicate: "contains".into(),
                to: id,
                status: RelationStatus::Active,
                valid_from: now,
                valid_until: None,
                sources: empty_sources.clone(),
            });
        }

        // Tech stack entities
        let techs = [
            ("rust", "Rust", "Language"),
            ("typescript", "TypeScript", "Language"),
            ("react", "React", "Framework"),
            ("axum", "Axum", "Framework"),
            ("sqlite", "SQLite", "Database"),
            ("oxigraph", "Oxigraph", "GraphDB"),
            ("onnx", "ONNX", "Runtime"),
            ("tauri", "Tauri", "Framework"),
            ("tokio", "Tokio", "Runtime"),
            ("reqwest", "reqwest", "Library"),
            ("sqlx", "SQLx", "Library"),
        ];
        for (id, label, kind) in &techs {
            self.upsert_entity(Entity {
                id: id.to_string(),
                label: label.to_string(),
                entity_type: EntityType::Other(kind.to_string()),
                properties: empty_props.clone(),
                sources: empty_sources.clone(),
                created_at: now,
                updated_at: now,
                merged_into: None,
            });
            self.add_relation(Relation {
                from: "everevo".into(),
                predicate: "uses".into(),
                to: id.to_string(),
                status: RelationStatus::Active,
                valid_from: now,
                valid_until: None,
                sources: empty_sources.clone(),
            });
        }

        tracing::info!(
            entities = self.entity_count(),
            relations = self.relation_count(),
            "Knowledge graph seeded with project structure"
        );
    }

    // ── Graph Operations ──────────────────────────────────────────────
    pub fn expand(&self, start_id: &str, depth: usize) -> Vec<Entity> {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut frontier: Vec<String> = vec![start_id.to_string()];
        let mut result: Vec<Entity> = Vec::new();
        for _ in 0..=depth {
            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                if !visited.insert(id.clone()) {
                    continue;
                }
                if let Some(e) = self.entities.get(id) {
                    result.push(e.clone());
                }
                for r in self.outgoing(id) {
                    if !visited.contains(&r.to) {
                        next.push(r.to.clone());
                    }
                }
                for r in self.incoming(id) {
                    if !visited.contains(&r.from) {
                        next.push(r.from.clone());
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        result
    }
    pub fn merge_entities(&mut self, canonical_id: &str, merged_id: &str) {
        if canonical_id == merged_id {
            return;
        }
        let merged = match self.entities.get(merged_id) {
            Some(e) => e.clone(),
            None => return,
        };
        if let Some(canonical) = self.entities.get_mut(canonical_id) {
            for (k, v) in &merged.properties {
                canonical
                    .properties
                    .entry(k.clone())
                    .or_insert_with(|| v.clone());
            }
            for src in &merged.sources {
                if !canonical
                    .sources
                    .iter()
                    .any(|s| s.message_id == src.message_id)
                {
                    canonical.sources.push(src.clone());
                }
            }
            canonical.updated_at = chrono::Utc::now();
        }
        for rel in self.relations.iter_mut() {
            if rel.from == *merged_id {
                rel.from = canonical_id.to_string();
            }
            if rel.to == *merged_id {
                rel.to = canonical_id.to_string();
            }
        }
        if let Some(merged_entity) = self.entities.get_mut(merged_id) {
            merged_entity.merged_into = Some(canonical_id.to_string());
            merged_entity.updated_at = chrono::Utc::now();
        }
    }
    pub fn resolve_all(&mut self) -> ResolveStats {
        let resolver = EntityResolver::default();
        resolver.resolve(self)
    }
}
