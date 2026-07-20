//! Knowledge Graph API routes — SPARQL query + entity lookup.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::app_state::AppState;
use everevo_core::EverEvoError;
use everevo_kg::KnowledgeGraph;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/kg/query", post(query_sparql))
        .route("/api/kg/entity/{name}", get(get_entity))
}

#[derive(Debug, Deserialize)]
struct SparqlRequest {
    query: String,
}

async fn query_sparql(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SparqlRequest>,
) -> Result<Json<serde_json::Value>, KgError> {
    let graph_dir = state.config.data_dir.join("memory").join("graph");
    let kg = KnowledgeGraph::open(&graph_dir)?;
    let rows = kg.query_sparql(&req.query)?;
    Ok(Json(serde_json::json!({ "results": rows, "count": rows.len() })))
}

async fn get_entity(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, KgError> {
    let graph_dir = state.config.data_dir.join("memory").join("graph");
    let kg = KnowledgeGraph::open(&graph_dir)?;

    // Search by name or ID
    let entities = kg.search(&name);
    if entities.is_empty() {
        return Err(KgError::not_found(name));
    }

    let entity = &entities[0];
    let outgoing: Vec<serde_json::Value> = kg.outgoing(&entity.id).iter().map(|r| {
        serde_json::json!({
            "predicate": r.predicate, "to": r.to,
            "status": if r.status == everevo_kg::RelationStatus::Active { "active" } else { "superseded" },
        })
    }).collect();
    let incoming: Vec<serde_json::Value> = kg.incoming(&entity.id).iter().map(|r| {
        serde_json::json!({
            "predicate": r.predicate, "from": r.from,
            "status": if r.status == everevo_kg::RelationStatus::Active { "active" } else { "superseded" },
        })
    }).collect();

    Ok(Json(serde_json::json!({
        "id": entity.id,
        "label": entity.label,
        "type": entity.entity_type.as_str(),
        "properties": entity.properties,
        "relations_out": outgoing,
        "relations_in": incoming,
        "merged_into": entity.merged_into,
        "created_at": entity.created_at.to_rfc3339(),
    })))
}

// ── Error ──────────────────────────────────────────────────────────────

struct KgError {
    status: axum::http::StatusCode,
    message: String,
}

impl KgError {
    fn not_found(name: String) -> Self {
        Self { status: axum::http::StatusCode::NOT_FOUND, message: format!("Entity not found: {name}") }
    }
}

impl From<EverEvoError> for KgError {
    fn from(e: EverEvoError) -> Self {
        Self { status: axum::http::StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() }
    }
}

impl axum::response::IntoResponse for KgError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}
