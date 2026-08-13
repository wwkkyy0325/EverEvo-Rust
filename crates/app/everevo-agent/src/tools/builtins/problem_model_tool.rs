//! ProblemModelTool — lets the agent build and manage a session-scoped problem
//! model (structural causal draft) for hard questions.
//!
//! Actions (audit LOW 2026-08-13: `add_nodes` batch was implemented but missing
//! from this doc):
//! - `init` — reset / create the model for this session.
//! - `add_node {id, kind, content, status?, source?}` — add a node
//!   (sub-question / fact / claim / candidate / constraint).
//! - `add_nodes {nodes: [...]}` — add MANY nodes in one call (prefer over
//!   repeated add_node — the modeling stage caps the model at ≤5 nodes).
//! - `add_edge {from, to, relation}` — add a causal / dependency / evidence /
//!   contradicts edge.
//! - `update_status {id, status, source?}` — mark a node Verified / Unverified /
//!   Unknown with an evidence source (the epistemic boundary from ADR 0009).
//! - `list` — return the current model snapshot (the default when `action` is
//!   omitted — a safe, recoverable default).
//! - `finalize` — mark the model as finalized (before committing).
//!
//! Main-loop only (like `ask_user`). Moved from the server crate during the
//! P1.1 tool-ownership refactor: the session-scoped store now comes from
//! [`crate::tools::session_store::SessionStore`].

use std::sync::Arc;

use uuid::Uuid;

use everevo_core::problem_model::{
    node_exists, EdgeKind, EpiStatus, NodeKind, ProblemEdge, ProblemModel, ProblemNode,
};

use crate::tools::session_store::SessionStore;

/// Session-scoped problem-model tool.
pub struct ProblemModelTool {
    pub session_id: Uuid,
    /// Session-scoped state provided by the server (the problem-model store).
    pub store: Arc<dyn SessionStore>,
}

#[async_trait::async_trait]
impl everevo_core::tool::Tool for ProblemModelTool {
    fn name(&self) -> &str {
        "problem_model"
    }
    fn description(&self) -> &str {
        "Build a session-scoped PROBLEM MODEL (causal draft) for a hard question. \
         Add nodes (sub-questions / facts / claims / candidates / constraints) with an \
         epistemic status (VERIFIED / UNVERIFIED / UNKNOWN), link them with causal / \
         dependency / evidence edges, then `finalize` before committing. Use for complex \
         or compound questions where a systematic, evidence-traced answer beats a single \
         pass."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["init", "add_node", "add_nodes", "add_edge", "update_status", "list", "finalize"],
                    "description": "What to do with the problem model. If omitted, defaults to 'list'."
                },
                "id": {"type": "string", "description": "Node id, e.g. 'q1', 'f1'."},
                "kind": {"type": "string", "enum": ["sub_question", "fact", "claim", "candidate", "constraint"]},
                "content": {"type": "string", "description": "Node content."},
                "status": {"type": "string", "enum": ["verified", "unverified", "unknown"]},
                "source": {"type": "string", "description": "Evidence source (tool result / file:line)."},
                "nodes": {"type": "array", "items": {"type": "object", "description": "A node: {id, kind, content, status?, source?}"}, "description": "Batch of nodes for add_nodes (prefer this over repeated add_node calls)."},
                "from": {"type": "string", "description": "Edge tail node id."},
                "to": {"type": "string", "description": "Edge head node id."},
                "relation": {"type": "string", "enum": ["causal", "dependency", "evidence", "contradicts"]}
            },
            "required": ["action"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel {
        everevo_core::types::RiskLevel::Low
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        // Empty / missing action defaults to `list` — a safe, recoverable
        // behavior instead of an error the agent must retry around.
        let action = params["action"].as_str().unwrap_or("list");
        let store = self.store.problem_models();
        let mut models = store.write().await;
        let model = models.entry(self.session_id).or_default();

        let output = match action {
            "init" => {
                *model = ProblemModel::default();
                "Problem model initialized (cleared).".into()
            }
            "add_node" => {
                let id = params["id"].as_str().unwrap_or("").trim().to_string();
                if id.is_empty() {
                    return Err(everevo_core::EverEvoError::InvalidInput(
                        "add_node requires `id`".into(),
                    ));
                }
                if node_exists(model, &id) {
                    return Err(everevo_core::EverEvoError::InvalidInput(format!(
                        "node id `{id}` already exists — use update_status"
                    )));
                }
                let kind = parse_kind(params["kind"].as_str());
                let content = params["content"].as_str().unwrap_or("").trim().to_string();
                let status = parse_status(params["status"].as_str()).unwrap_or_default();
                let source = params["source"].as_str().map(|s| s.to_string());
                let msg = format!("Added node `{id}` ({}).", kind.as_str());
                model.nodes.push(ProblemNode {
                    id,
                    kind,
                    content,
                    status,
                    source,
                });
                msg
            }
            "add_nodes" => {
                let nodes = params["nodes"].as_array().ok_or_else(|| {
                    everevo_core::EverEvoError::InvalidInput(
                        "add_nodes requires `nodes` (array of {id, kind, content, status?, source?})"
                            .into(),
                    )
                })?;
                let mut added = 0usize;
                for n in nodes {
                    let id = n["id"].as_str().unwrap_or("").trim().to_string();
                    if id.is_empty() || node_exists(model, &id) {
                        continue; // skip invalid/duplicate ids silently
                    }
                    let kind = parse_kind(n["kind"].as_str());
                    let content = n["content"].as_str().unwrap_or("").trim().to_string();
                    let status = parse_status(n["status"].as_str()).unwrap_or_default();
                    let source = n["source"].as_str().map(|s| s.to_string());
                    model.nodes.push(ProblemNode {
                        id,
                        kind,
                        content,
                        status,
                        source,
                    });
                    added += 1;
                }
                format!("Added {added} nodes in one call.")
            }
            "add_edge" => {
                let from = params["from"].as_str().unwrap_or("").trim().to_string();
                let to = params["to"].as_str().unwrap_or("").trim().to_string();
                if !node_exists(model, &from) || !node_exists(model, &to) {
                    return Err(everevo_core::EverEvoError::InvalidInput(
                        "add_edge requires both `from` and `to` to be existing node ids".into(),
                    ));
                }
                let relation = parse_edge(params["relation"].as_str()).unwrap_or_default();
                let msg = format!("Added edge `{from}` --{}--> `{to}`.", relation.as_str());
                model.edges.push(ProblemEdge { from, to, relation });
                msg
            }
            "update_status" => {
                let id = params["id"].as_str().unwrap_or("").trim().to_string();
                let status = parse_status(params["status"].as_str()).ok_or_else(|| {
                    everevo_core::EverEvoError::InvalidInput(
                        "update_status requires `status` in verified|unverified|unknown".into(),
                    )
                })?;
                let node = model.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| {
                    everevo_core::EverEvoError::InvalidInput(format!("node `{id}` not found"))
                })?;
                node.status = status;
                if let Some(src) = params["source"].as_str() {
                    node.source = Some(src.to_string());
                }
                format!("Node `{id}` marked [{}].", status.as_str())
            }
            "list" => {
                if model.is_empty() {
                    "Problem model is empty — use `add_node` to start building the causal draft."
                        .into()
                } else {
                    model.render()
                }
            }
            "finalize" => {
                if model.is_empty() {
                    return Err(everevo_core::EverEvoError::InvalidInput(
                        "cannot finalize an empty problem model — build it first".into(),
                    ));
                }
                model.finalized = true;
                "Problem model finalized. Answer each sub-question with its [VERIFIED] evidence."
                    .into()
            }
            other => {
                return Err(everevo_core::EverEvoError::InvalidInput(format!(
                    "unknown action `{other}` — expected init|add_node|add_nodes|add_edge|update_status|list|finalize"
                )));
            }
        };

        Ok(everevo_core::tool::ToolOutput::text(output))
    }
}

fn parse_kind(s: Option<&str>) -> NodeKind {
    match s {
        Some("sub_question") => NodeKind::SubQuestion,
        Some("fact") => NodeKind::Fact,
        Some("candidate") => NodeKind::Candidate,
        Some("constraint") => NodeKind::Constraint,
        _ => NodeKind::Claim,
    }
}

fn parse_status(s: Option<&str>) -> Option<EpiStatus> {
    match s {
        Some("verified") => Some(EpiStatus::Verified),
        Some("unverified") => Some(EpiStatus::Unverified),
        Some("unknown") => Some(EpiStatus::Unknown),
        _ => None,
    }
}

fn parse_edge(s: Option<&str>) -> Option<EdgeKind> {
    match s {
        Some("causal") => Some(EdgeKind::Causal),
        Some("dependency") => Some(EdgeKind::Dependency),
        Some("evidence") => Some(EdgeKind::Evidence),
        Some("contradicts") => Some(EdgeKind::Contradicts),
        _ => None,
    }
}
