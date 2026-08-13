//! Config API — multi-provider LLM settings with persistence.

use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::app_state::AppState;
use everevo_core::llm::LlmProvider;
use everevo_core::ApiError;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LlmProviderConfig {
    #[serde(default = "default_id")]
    pub id: String,
    pub api_format: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Provider context window in tokens (optional). Used by context
    /// compaction to budget chunk summarization (D1). Vision providers must
    /// stay ≤ 32K to protect VRAM on the 6GB local GPU.
    #[serde(default)]
    pub context_window: Option<u32>,
}

fn default_id() -> String {
    "primary".into()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoutingSettings {
    #[serde(default)]
    #[serde(rename = "mainModelId")]
    pub main_model_id: Option<String>,
    #[serde(default)]
    #[serde(rename = "mainEffort")]
    pub main_effort: Option<String>,
    /// Provider id used for image description (`describe_image` tool).
    /// Falls back to a `[[llm]]` entry with id "vision" if unset.
    #[serde(default)]
    #[serde(rename = "visionModelId")]
    pub vision_model_id: Option<String>,
    /// Provider id used for context compaction / rolling summary.
    /// Unset → falls back to the main execution model ("有哪个用哪个").
    #[serde(default)]
    #[serde(rename = "compactModelId")]
    pub compact_model_id: Option<String>,
    #[serde(default)]
    pub tiers: Vec<CascadeTier>,
    /// Meta-agent self-diagnosis toggle. Product default ON; benchmark mode
    /// (EVEREVO_BENCHMARK) defaults OFF unless `EVEREVO_META_AGENT=1` is set.
    #[serde(default = "default_meta_agent_enabled")]
    #[serde(rename = "metaAgentEnabled")]
    pub meta_agent_enabled: bool,
}

fn default_meta_agent_enabled() -> bool {
    true
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            main_model_id: None,
            main_effort: None,
            vision_model_id: None,
            compact_model_id: None,
            tiers: Vec::new(),
            meta_agent_enabled: true,
        }
    }
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
    /// MCP server definitions — round-tripped through the config UI so manual
    /// edits to `[[mcp_servers]]` in config.toml aren't destroyed on save.
    #[serde(default)]
    pub mcp_servers: Option<Vec<everevo_core::config::McpServerConfig>>,
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
                "context_window": p.context_window,
            })
        })
        .collect();
    let has_llm = state.llm.read().await.values().any(|c| c.is_some());
    Json(serde_json::json!({ "llm": providers, "has_llm": has_llm }))
}

async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AppSettings>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let providers = body.llm.clone().unwrap_or_default();
    let existing = load_settings(&state).await;
    let merged = AppSettings {
        llm: body.llm,
        routing: existing.routing, // keep routing intact
        mcp_servers: body.mcp_servers.or(existing.mcp_servers), // round-trip MCP
    };
    let path = state.config.data_dir.join("config.toml");
    let toml_str = toml::to_string_pretty(&merged).unwrap_or_default();
    match tokio::fs::write(&path, toml_str).await {
        Ok(_) => {
            // Auto-reload LLM clients from the saved config
            let mut new_map: HashMap<String, Option<Arc<everevo_agent::llm::HttpClient>>> =
                HashMap::new();
            for p in &providers {
                // Local providers (e.g. llama-server vision) have no api_key —
                // a base_url alone is enough to build a client.
                if !p.api_key.is_empty() || !p.base_url.is_empty() {
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
            state.resolve_special_providers().await;
            state.resolve_main_provider().await;
            state.resolve_web_search_provider().await;

            if !new_map.is_empty() {
                state.llm_notify.notify_one();
            }
            Ok(Json(serde_json::json!({ "data": { "saved": true } })))
        }
        Err(e) => Err(ApiError::internal(format!("Save failed: {e}"))),
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
    state.resolve_special_providers().await;
    state.resolve_main_provider().await;
    state.resolve_web_search_provider().await;

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
        "visionModelId": routing.vision_model_id.unwrap_or_default(),
        "compactModelId": routing.compact_model_id.unwrap_or_default(),
        "metaAgentEnabled": routing.meta_agent_enabled,
        "tiers": tiers,
    }))
}

async fn put_routing(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    let vision_model_id = body
        .get("visionModelId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let compact_model_id = body
        .get("compactModelId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // Preserve the current value when the incoming body omits it (old clients),
    // so saving routing config never silently re-enables/disables meta-agent.
    let meta_agent_enabled = body
        .get("metaAgentEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            settings
                .routing
                .as_ref()
                .map(|r| r.meta_agent_enabled)
                .unwrap_or(true)
        });

    settings.routing = Some(RoutingSettings {
        tiers,
        main_model_id,
        main_effort,
        vision_model_id,
        compact_model_id,
        meta_agent_enabled,
    });
    match save_settings(&state, &settings).await {
        Ok(_) => {
            state.resolve_special_providers().await;
            state.resolve_main_provider().await;
            state.resolve_web_search_provider().await;
            *state.meta_agent_enabled.write().await = meta_agent_enabled;
            Ok(Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => Err(ApiError::internal(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_settings_roundtrip_new_fields() {
        let settings = RoutingSettings {
            main_model_id: Some("deepseek".into()),
            main_effort: Some("high".into()),
            vision_model_id: Some("vision".into()),
            compact_model_id: Some("compact".into()),
            tiers: vec![],
            meta_agent_enabled: false,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: RoutingSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.vision_model_id.as_deref(), Some("vision"));
        assert_eq!(parsed.compact_model_id.as_deref(), Some("compact"));
        assert_eq!(parsed.main_model_id.as_deref(), Some("deepseek"));
        assert!(!parsed.meta_agent_enabled);
        // Exact JSON keys (camelCase) that the frontend PUT/GET round-trips.
        assert!(json.contains("\"visionModelId\":\"vision\""));
        assert!(json.contains("\"compactModelId\":\"compact\""));
        assert!(json.contains("\"metaAgentEnabled\":false"));

        // Absent fields default to None / true (non-breaking for old clients).
        let parsed: RoutingSettings = serde_json::from_str("{\"mainModelId\":\"x\"}").unwrap();
        assert_eq!(parsed.vision_model_id, None);
        assert_eq!(parsed.compact_model_id, None);
        assert!(parsed.meta_agent_enabled);
        // Manual Default keeps product default ON.
        assert!(RoutingSettings::default().meta_agent_enabled);
    }

    #[test]
    fn provider_config_context_window_roundtrip() {
        let p = LlmProviderConfig {
            id: "vision".into(),
            api_format: "openai".into(),
            api_key: String::new(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            model: "qwen3-vl-2b-instruct".into(),
            context_window: Some(32_768),
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: LlmProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.context_window, Some(32_768));
        // Old client without the field → None (non-breaking).
        let parsed: LlmProviderConfig =
            serde_json::from_str("{\"id\":\"x\",\"api_format\":\"openai\",\"api_key\":\"\",\"base_url\":\"\",\"model\":\"m\"}")
                .unwrap();
        assert_eq!(parsed.context_window, None);
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

// Credential config removed — sandbox inherits host git config directly.
