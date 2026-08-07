//! Model management routes.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use everevo_core::ApiError;

type HandlerResult<T> = std::result::Result<T, ApiError>;

fn err<E: ToString>(e: E) -> ApiError {
    ApiError::internal(e.to_string())
}

#[derive(Serialize)]
struct ModelInfo {
    name: String,
    display_name: String,
    dim: usize,
    active: bool,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
    active: String,
    active_dim: usize,
}

/// GET /api/models
async fn list_models(State(state): State<Arc<AppState>>) -> Json<ModelsResponse> {
    let reg = state
        .model_registry
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let active = reg.active();
    Json(ModelsResponse {
        models: reg
            .list()
            .iter()
            .map(|m| ModelInfo {
                name: m.name.clone(),
                display_name: m.display_name.clone(),
                dim: m.dim,
                active: m.active,
            })
            .collect(),
        active: active.map(|a| a.name.clone()).unwrap_or_default(),
        active_dim: active.map(|a| a.dim).unwrap_or(0),
    })
}

#[derive(Deserialize)]
struct ActivateRequest {
    model: String,
}

/// POST /api/models/activate
async fn activate_model(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ActivateRequest>,
) -> HandlerResult<Json<ModelInfo>> {
    let mut reg = state.model_registry.write().map_err(err)?;
    let meta = reg.activate(&req.model).map_err(err)?;
    Ok(Json(ModelInfo {
        name: meta.name,
        display_name: meta.display_name,
        dim: meta.dim,
        active: true,
    }))
}

#[derive(Serialize)]
struct ReindexResponse {
    collection: String,
    processed: usize,
    duration_ms: u64,
}

/// POST /api/vector/reindex
async fn reindex_collection(
    State(state): State<Arc<AppState>>,
    Json(req): Json<serde_json::Value>,
) -> HandlerResult<Json<ReindexResponse>> {
    let collection = req
        .get("collection")
        .and_then(|v| v.as_str())
        .unwrap_or("memory");
    let rag = state.rag_pipeline.as_ref()
        .ok_or_else(|| ApiError::bad_request("RAG pipeline not initialized"))?;

    let start = std::time::Instant::now();
    let count = state.fact_manager.index_into_rag(rag).map_err(err)?;
    Ok(Json(ReindexResponse {
        collection: collection.to_string(),
        processed: count,
        duration_ms: start.elapsed().as_millis() as u64,
    }))
}

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/models", axum::routing::get(list_models))
        .route("/api/models/activate", axum::routing::post(activate_model))
        .route(
            "/api/vector/reindex",
            axum::routing::post(reindex_collection),
        )
}
