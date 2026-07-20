//! Config API — multi-provider LLM settings with persistence.

use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::app_state::AppState;
use everevo_core::llm::LlmProvider;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LlmProviderConfig {
    #[serde(default = "default_id")]
    pub id: String,
    pub api_format: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

fn default_id() -> String { "primary".into() }

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AppSettings {
    pub llm: Option<Vec<LlmProviderConfig>>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/verify", get(verify_config))
        .route("/api/config/reload", get(reload_llm))
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let providers: Vec<_> = settings.llm.unwrap_or_default().iter().map(|p| {
        let masked = if p.api_key.len() > 4 { format!("{}...{}", &p.api_key[..4], &p.api_key[p.api_key.len()-4..]) } else { "not set".into() };
        serde_json::json!({
            "id": p.id, "api_format": p.api_format,
            "api_key": masked, "base_url": p.base_url, "model": p.model,
        })
    }).collect();
    let has_llm = state.llm.read().await.values().any(|c| c.is_some());
    Json(serde_json::json!({ "llm": providers, "has_llm": has_llm }))
}

async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AppSettings>,
) -> Json<serde_json::Value> {
    let path = state.config.data_dir.join("config.toml");
    let toml_str = toml::to_string_pretty(&body).unwrap_or_default();
    match tokio::fs::write(&path, toml_str).await {
        Ok(_) => Json(serde_json::json!({ "data": { "saved": true } })),
        Err(e) => Json(serde_json::json!({ "error": format!("Save failed: {e}") })),
    }
}

async fn verify_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let guard = state.llm.read().await;
    let client = match guard.get("primary").and_then(|c| c.as_ref()).or_else(|| guard.values().find_map(|c| c.as_ref())) {
        Some(c) => c.clone(),
        None => return Json(serde_json::json!({ "ok": false, "error": "未配置模型，请先保存" })),
    };
    drop(guard);

    match client.chat(&[everevo_core::llm::LlmMessage::user("ping")], &[]).await {
        Ok(r) => Json(serde_json::json!({ "ok": true, "response": r.content.unwrap_or_default() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn reload_llm(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let providers = settings.llm.unwrap_or_default();
    let mut new_map: HashMap<String, Option<Arc<everevo_agent::llm::HttpClient>>> = HashMap::new();

    for p in &providers {
        let client = everevo_agent::llm::HttpClient::new(&p.api_format, &p.api_key, &p.base_url, &p.model);
        new_map.insert(p.id.clone(), Some(Arc::new(client)));
    }

    let mut guard = state.llm.write().await;
    for (id, client) in &new_map { guard.insert(id.clone(), client.clone()); }
    // Remove providers that no longer exist
    guard.retain(|id, _| new_map.contains_key(id));
    drop(guard);

    Json(serde_json::json!({ "data": { "reloaded": true, "providers": providers.len() } }))
}

async fn load_settings(state: &AppState) -> AppSettings {
    let path = state.config.data_dir.join("config.toml");
    tokio::fs::read_to_string(&path).await.ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}
