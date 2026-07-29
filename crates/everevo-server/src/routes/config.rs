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

fn default_id() -> String {
    "primary".into()
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct RoutingSettings {
    #[serde(default)]
    #[serde(rename = "mainModelId")]
    pub main_model_id: Option<String>,
    #[serde(default)]
    #[serde(rename = "mainEffort")]
    pub main_effort: Option<String>,
    #[serde(default)]
    pub tiers: Vec<CascadeTier>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CascadeTier {
    #[serde(default)]
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default)]
    pub effort: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AppSettings {
    pub llm: Option<Vec<LlmProviderConfig>>,
    #[serde(default)]
    pub routing: Option<RoutingSettings>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/verify", get(verify_config))
        .route("/api/config/reload", get(reload_llm))
        .route("/api/balance", get(get_balance))
        .route("/api/routing", get(get_routing).put(put_routing))
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let providers: Vec<_> = settings
        .llm
        .unwrap_or_default()
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id, "api_format": p.api_format,
                "api_key": p.api_key, "base_url": p.base_url, "model": p.model,
            })
        })
        .collect();
    let has_llm = state.llm.read().await.values().any(|c| c.is_some());
    Json(serde_json::json!({ "llm": providers, "has_llm": has_llm }))
}

async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AppSettings>,
) -> Json<serde_json::Value> {
    let providers = body.llm.clone().unwrap_or_default();
    let existing = load_settings(&state).await;
    let merged = AppSettings {
        llm: body.llm,
        routing: existing.routing, // keep routing intact
    };
    let path = state.config.data_dir.join("config.toml");
    let toml_str = toml::to_string_pretty(&merged).unwrap_or_default();
    match tokio::fs::write(&path, toml_str).await {
        Ok(_) => {
            // Auto-reload LLM clients from the saved config
            let mut new_map: HashMap<String, Option<Arc<everevo_agent::llm::HttpClient>>> =
                HashMap::new();
            for p in &providers {
                if !p.api_key.is_empty() {
                    let client = everevo_agent::llm::HttpClient::new(
                        &p.api_format,
                        &p.api_key,
                        &p.base_url,
                        &p.model,
                    );
                    new_map.insert(p.id.clone(), Some(Arc::new(client)));
                }
            }
            let mut guard = state.llm.write().await;
            for (id, client) in &new_map {
                guard.insert(id.clone(), client.clone());
            }
            guard.retain(|id, _| new_map.contains_key(id));
            drop(guard);

            if !new_map.is_empty() {
                state.llm_notify.notify_one();
            }
            Json(serde_json::json!({ "data": { "saved": true } }))
        }
        Err(e) => Json(serde_json::json!({ "error": format!("Save failed: {e}") })),
    }
}

async fn verify_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let guard = state.llm.read().await;
    let client = match guard
        .get("primary")
        .and_then(|c| c.as_ref())
        .or_else(|| guard.values().find_map(|c| c.as_ref()))
    {
        Some(c) => c.clone(),
        None => return Json(serde_json::json!({ "ok": false, "error": "未配置模型，请先保存" })),
    };
    drop(guard);

    match client
        .chat(&[everevo_core::llm::LlmMessage::user("ping")], &[])
        .await
    {
        Ok(r) => Json(serde_json::json!({ "ok": true, "response": r.content.unwrap_or_default() })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn reload_llm(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let providers = settings.llm.unwrap_or_default();
    let mut new_map: HashMap<String, Option<Arc<everevo_agent::llm::HttpClient>>> = HashMap::new();

    for p in &providers {
        let client =
            everevo_agent::llm::HttpClient::new(&p.api_format, &p.api_key, &p.base_url, &p.model);
        new_map.insert(p.id.clone(), Some(Arc::new(client)));
    }

    let mut guard = state.llm.write().await;
    for (id, client) in &new_map {
        guard.insert(id.clone(), client.clone());
    }
    // Remove providers that no longer exist
    guard.retain(|id, _| new_map.contains_key(id));
    drop(guard);

    // Wake the init pipeline if it's waiting for LLM config
    if !providers.is_empty() {
        state.llm_notify.notify_one();
    }

    Json(serde_json::json!({ "data": { "reloaded": true, "providers": providers.len() } }))
}

async fn load_settings(state: &AppState) -> AppSettings {
    let path = state.config.data_dir.join("config.toml");
    tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

async fn save_settings(state: &AppState, settings: &AppSettings) -> Result<(), String> {
    let path = state.config.data_dir.join("config.toml");
    let toml_str = toml::to_string_pretty(settings).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, toml_str)
        .await
        .map_err(|e| e.to_string())
}

// ── Routing config ──────────────────────────────────────────────────────

async fn get_routing(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let routing = settings.routing.unwrap_or_default();
    let tiers: Vec<_> = routing
        .tiers
        .iter()
        .map(|t| {
            serde_json::json!({
                "modelId": t.model_id, "effort": t.effort,
            })
        })
        .collect();
    Json(serde_json::json!({
        "mainModelId": routing.main_model_id.unwrap_or_default(),
        "mainEffort": routing.main_effort.unwrap_or_else(|| "auto".to_string()),
        "tiers": tiers,
    }))
}

async fn put_routing(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let mut settings = load_settings(&state).await;
    let tiers: Vec<CascadeTier> = body
        .get("tiers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| CascadeTier {
                    model_id: t
                        .get("modelId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    effort: t
                        .get("effort")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let main_model_id = body
        .get("mainModelId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let main_effort = body
        .get("mainEffort")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    settings.routing = Some(RoutingSettings {
        tiers,
        main_model_id,
        main_effort,
    });
    match save_settings(&state, &settings).await {
        Ok(_) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

// ── Balance check — proxies DeepSeek /user/balance per provider ────────

async fn get_balance(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let settings = load_settings(&state).await;
    let providers = settings.llm.unwrap_or_default();
    let client = reqwest::Client::new();

    let mut results = Vec::new();

    for p in &providers {
        if p.api_key.is_empty() {
            results.push(
                serde_json::json!({ "provider_id": p.id, "ok": false, "error": "未配置 API Key" }),
            );
            continue;
        }

        // Use origin only — balance endpoint lives at root, not under API-format paths like /anthropic
        let url = match reqwest::Url::parse(&p.base_url) {
            Ok(parsed) => format!("{}://{}/user/balance", parsed.scheme(), parsed.authority()),
            Err(_) => format!("{}/user/balance", p.base_url.trim_end_matches('/')),
        };
        match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", p.api_key))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                match resp.text().await {
                    Ok(body) => {
                        if status == 200 {
                            match serde_json::from_str::<serde_json::Value>(&body) {
                                Ok(json) => results.push(serde_json::json!({
                                    "provider_id": p.id, "ok": true, "data": json,
                                })),
                                Err(_) => results.push(serde_json::json!({
                                    "provider_id": p.id, "ok": false, "error": "解析响应失败",
                                })),
                            }
                        } else {
                            results.push(serde_json::json!({
                                "provider_id": p.id, "ok": false, "error": format!("HTTP {status}"),
                            }));
                        }
                    }
                    Err(e) => results.push(serde_json::json!({
                        "provider_id": p.id, "ok": false, "error": e.to_string(),
                    })),
                }
            }
            Err(e) => results.push(serde_json::json!({
                "provider_id": p.id, "ok": false, "error": e.to_string(),
            })),
        }
    }

    Json(serde_json::json!({ "balances": results }))
}
