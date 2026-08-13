//! Knowledge/memory routes — grouped micro-route modules (2026-08-13 restructure).
use crate::app_state::AppState;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use everevo_core::ApiError;
use serde::Deserialize;
use std::sync::Arc;

fn kg_routes_router() -> Router<Arc<AppState>> {
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
) -> Result<Json<serde_json::Value>, ApiError> {
    let kg = state
        .knowledge_graph
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let rows = kg.query_sparql(&req.query)?;
    Ok(Json(
        serde_json::json!({ "results": rows, "count": rows.len() }),
    ))
}

async fn get_entity(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let kg = state
        .knowledge_graph
        .read()
        .unwrap_or_else(|e| e.into_inner());

    // Search by name or ID
    let entities = kg.search(&name);
    if entities.is_empty() {
        return Err(ApiError::not_found(name));
    }

    let entity = &entities[0];
    let outgoing: Vec<serde_json::Value> = kg.outgoing(&entity.id).iter().map(|r| {
        serde_json::json!({
            "predicate": r.predicate, "to": r.to,
            "status": if r.status == everevo_knowledge::graph::RelationStatus::Active { "active" } else { "superseded" },
        })
    }).collect();
    let incoming: Vec<serde_json::Value> = kg.incoming(&entity.id).iter().map(|r| {
        serde_json::json!({
            "predicate": r.predicate, "from": r.from,
            "status": if r.status == everevo_knowledge::graph::RelationStatus::Active { "active" } else { "superseded" },
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

#[derive(Deserialize, Default)]
struct DiaryQuery {
    date: Option<String>,
}

fn diary_routes_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/diary", axum::routing::get(list_or_read))
        .route("/api/diary/today", axum::routing::get(today))
}

async fn list_or_read(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DiaryQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(date) = q.date {
        match state.diary_manager.read_date(&date) {
            Ok(content) => Ok(Json(serde_json::json!({
                "date": date,
                "content": content,
                "has_content": !content.is_empty(),
            }))),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    } else {
        // List recent diary files
        match state.diary_manager.read_recent(14) {
            Ok(files) => {
                let entries: Vec<_> = files
                    .into_iter()
                    .map(|(date, content)| {
                        let excerpt: String = content
                            .lines()
                            .filter(|l| !l.starts_with('#') && !l.is_empty())
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(" ");
                        serde_json::json!({
                            "date": date,
                            "size": content.len(),
                            "excerpt": excerpt.chars().take(200).collect::<String>(),
                        })
                    })
                    .collect();
                Ok(Json(serde_json::json!({"files": entries})))
            }
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }
}

async fn today(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    match state.diary_manager.read_today() {
        Ok(content) => Ok(Json(serde_json::json!({
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "content": content,
            "has_content": !content.is_empty(),
        }))),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

pub fn router() -> axum::Router<Arc<crate::app_state::AppState>> {
    axum::Router::new()
        .merge(kg_routes_router())
        .merge(diary_routes_router())
}
