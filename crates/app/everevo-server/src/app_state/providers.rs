use std::{collections::HashMap, sync::Arc};

use everevo_agent::llm::HttpClient;
use everevo_core::AppConfig;

use super::AppState;

/// A resolved special-purpose provider (vision / compaction) plus its optional
/// context window (tokens), used to budget context-maintenance summarization.
#[derive(Clone)]
pub struct ResolvedProvider {
    pub client: Arc<HttpClient>,
    pub context_window: Option<u32>,
}

impl AppState {
    /// Read LLM provider configs from `data/config.toml` and build HttpClient instances.
    /// Returns empty map if the file doesn't exist or is malformed — the bootstrap UI
    /// will prompt the user to configure providers.
    pub(crate) async fn load_llm_from_file(
        config: &AppConfig,
    ) -> HashMap<String, Option<Arc<HttpClient>>> {
        let path = config.data_dir.join("config.toml");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let table: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return HashMap::new(),
        };
        let llm_arr = match table.get("llm").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return HashMap::new(),
        };

        // Auto-detect proxy from env vars + common local proxy ports
        let proxy = everevo_agent::llm::http::detect_proxy().await;

        let mut map = HashMap::new();
        let mut plaintext_keys_found = false;
        for entry in llm_arr {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("primary");
            let api_fmt = entry
                .get("api_format")
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic");
            let key = entry.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
            let url = entry.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
            let model = entry.get("model").and_then(|v| v.as_str()).unwrap_or("");

            // Local providers (e.g. llama-server vision) have no api_key — a
            // base_url alone is enough to build a working client.
            if !key.is_empty() || !url.is_empty() {
                if !key.is_empty() {
                    plaintext_keys_found = true;
                }
                let client = HttpClient::with_proxy(api_fmt, key, url, model, proxy.as_deref());
                map.insert(id.to_string(), Some(Arc::new(client)));
            }
        }
        if plaintext_keys_found {
            tracing::warn!(
                "⚠️  API keys found in plaintext in data/config/config.toml. \
                 For better security, move keys to a .env file (loaded automatically) \
                 or environment variables. The config.toml file is readable by shell \
                 and read_file tools — plaintext keys are a security risk."
            );
        }

        // Honor `[routing] mainModelId`: re-key that provider as "primary" so the
        // chat handler deterministically uses the configured main model instead of
        // whichever entry HashMap iteration happens to return first. Without this,
        // a stale/duplicate [[llm]] block can silently become the active model.
        if let Some(main_id) = table
            .get("routing")
            .and_then(|r| r.get("mainModelId"))
            .and_then(|v| v.as_str())
        {
            if let Some(client) = map.get(main_id).cloned() {
                map.insert("primary".to_string(), client);
                tracing::info!(main_id, "Routing mainModelId set as primary LLM");
            } else {
                tracing::warn!(
                    main_id,
                    "routing.mainModelId not found among [[llm]] entries"
                );
            }
        }
        map
    }

    /// Resolve `vision_llm` / `compact_llm` from `[routing] visionModelId` /
    /// `compactModelId` (with `id == "vision"` fallback convention for vision).
    /// Reads config.toml fresh so it stays in sync after any PUT /api/config or
    /// /api/routing save. Idempotent — safe to call after every provider change.
    pub async fn resolve_special_providers(&self) {
        let path = self.config.data_dir.join("config.toml");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return,
        };
        let table: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return,
        };
        let llm_arr = match table.get("llm").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };

        let mut windows: HashMap<String, u32> = HashMap::new();
        for entry in llm_arr {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("primary")
                .to_string();
            if let Some(w) = entry.get("context_window").and_then(|v| v.as_integer()) {
                windows.insert(id, w.max(0) as u32);
            }
        }

        let routing = table.get("routing");
        let vision_id = routing
            .and_then(|r| r.get("visionModelId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                // Fallback convention: a [[llm]] entry with id "vision" is used
                // even when visionModelId isn't explicitly set.
                if llm_arr
                    .iter()
                    .any(|e| e.get("id").and_then(|v| v.as_str()) == Some("vision"))
                {
                    Some("vision".to_string())
                } else {
                    None
                }
            });
        let compact_id = routing
            .and_then(|r| r.get("compactModelId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let guard = self.llm.read().await;
        let resolve = |id: &str| -> Option<ResolvedProvider> {
            let client = guard.get(id).and_then(|c| c.clone())?;
            Some(ResolvedProvider {
                client,
                context_window: windows.get(id).copied(),
            })
        };
        let vision = vision_id.as_deref().and_then(resolve);
        let compact = compact_id.as_deref().and_then(resolve);
        drop(guard);

        let vision_resolved = vision.is_some();
        let compact_resolved = compact.is_some();
        *self.vision_llm.write().await = vision;
        *self.compact_llm.write().await = compact;
        if vision_resolved || compact_resolved {
            tracing::info!(
                "Special providers resolved: vision={}, compact={}",
                vision_id.unwrap_or_default(),
                compact_id.unwrap_or_default()
            );
        }
    }

    /// Resolve the main execution provider and its `context_window`, used to
    /// budget the primary chat session's context assembly.
    ///
    /// Reuses the same re-key rule as `load_llm_from_file`: `[routing]
    /// mainModelId` wins, falling back to the "primary" re-key. Reads
    /// config.toml fresh so it stays in sync after PUT /api/config or
    /// /api/routing. Idempotent.
    pub async fn resolve_main_provider(&self) {
        let path = self.config.data_dir.join("config.toml");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return,
        };
        let table: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return,
        };
        let llm_arr = match table.get("llm").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };

        let mut windows: HashMap<String, u32> = HashMap::new();
        for entry in llm_arr {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("primary")
                .to_string();
            if let Some(w) = entry.get("context_window").and_then(|v| v.as_integer()) {
                windows.insert(id, w.max(0) as u32);
            }
        }

        // Same precedence as load_llm_from_file: mainModelId, else "primary".
        let main_id = table
            .get("routing")
            .and_then(|r| r.get("mainModelId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "primary".to_string());

        let main = {
            let guard = self.llm.read().await;
            guard
                .get(&main_id)
                .or_else(|| guard.get("primary"))
                .and_then(|c| c.clone())
                .map(|client| ResolvedProvider {
                    client,
                    context_window: windows
                        .get(&main_id)
                        .or_else(|| windows.get("primary"))
                        .copied(),
                })
        };
        *self.main_llm.write().await = main;
        tracing::info!(main_id, "Main provider resolved");
    }

    /// Resolve the web-search delegate provider — the first Anthropic-format
    /// `[[llm]]` entry (e.g. DeepSeek) whose API natively executes server-side
    /// web search (`web_search_20250305`). When set, the in-process
    /// `web_search_local` tool delegates research to it instead of the plugin's
    /// cn.bing/Sogou chain. Reads config.toml fresh; idempotent.
    pub async fn resolve_web_search_provider(&self) {
        let path = self.config.data_dir.join("config.toml");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return,
        };
        let table: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return,
        };
        let llm_arr = match table.get("llm").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };

        let mut windows: HashMap<String, u32> = HashMap::new();
        for entry in llm_arr {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("primary")
                .to_string();
            if let Some(w) = entry.get("context_window").and_then(|v| v.as_integer()) {
                windows.insert(id, w.max(0) as u32);
            }
        }

        // First entry whose API format supports server-side native web search.
        // Default is "anthropic" (matches load_llm_from_file), so entries that
        // omit api_format count as candidates; OpenAI/llama-server do not.
        let ws_id = llm_arr
            .iter()
            .find(|e| {
                e.get("api_format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("anthropic")
                    == "anthropic"
            })
            .and_then(|e| e.get("id").and_then(|v| v.as_str()))
            .map(|s| s.to_string());

        let ws = {
            let guard = self.llm.read().await;
            ws_id
                .as_deref()
                .and_then(|id| guard.get(id).and_then(|c| c.clone()))
                .map(|client| ResolvedProvider {
                    client,
                    context_window: ws_id.as_deref().and_then(|id| windows.get(id).copied()),
                })
        };
        *self.web_search_llm.write().await = ws;
        if self.web_search_llm.read().await.is_some() {
            tracing::info!(
                ws_id = ws_id.unwrap_or_default(),
                "Web-search delegate provider resolved"
            );
        }
    }
}
