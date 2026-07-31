//! Application configuration and data directory resolution.
//!
//! # Data Directory
//!
//! All runtime data (DB, vectors, graph, sandbox, files) lives under `data/`
//! in the project root. The executable locates this relative to CWD.
//!
//! - Default: `./data/` (project root, dev & prod both)
//! - Override: `EVEREVO_DATA_DIR` environment variable

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::LlmProviderConfig;

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Root directory for all runtime data (DB, vectors, graph, sandbox, files).
    pub data_dir: PathBuf,

    /// Configuration file directory (computed as `data_dir.join("config")`).
    #[serde(skip)]
    pub config_dir: PathBuf,

    /// Server bind host.
    #[serde(default = "default_host")]
    pub server_host: String,

    /// Server bind port.
    #[serde(default = "default_port")]
    pub server_port: u16,

    /// Configured LLM providers (at least one required).
    pub llm_providers: Vec<LlmProviderConfig>,

    /// Which provider to use as default.
    pub default_provider: String,

    /// SQLite database path (relative to data_dir if not absolute).
    #[serde(default)]
    pub database_path: Option<String>,

    /// Maximum tokens for conversation context window.
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,

    /// Summarization trigger threshold (fraction of max_context_tokens).
    #[serde(default = "default_summarize_threshold")]
    pub summarize_threshold: f32,

    /// Default sandbox permission level for new sessions.
    /// "semi_auto" | "fully_auto" | "fully_manual" | "read_only"
    #[serde(default = "default_permission_level_str")]
    pub default_permission_level: String,

    /// Default workspace directory. When set, all new sessions use this
    /// as their primary working directory instead of the auto-created sandbox.
    /// Overridable per-session via PUT /api/workspace.
    #[serde(default)]
    pub workspace_dir: Option<PathBuf>,

    /// MCP (Model Context Protocol) servers to connect at startup.
    /// Each entry: { name, command, args[] }
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// Preferred embedding model name. Must match a subdirectory under
    /// `data/models/`. If not set, the first discovered model is used.
    #[serde(default)]
    pub embedding_model: Option<String>,
}

/// Configuration for an MCP server connection.
/// Mirrors Claude Code's `.mcp.json` format with three transport types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// Transport type: "stdio" (default), "sse" (Server-Sent Events), or "http" (Streamable HTTP).
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Command for stdio transport (e.g. "npx", "python"). Required for stdio.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// URL for HTTP/SSE transport (e.g. "https://mcp.example.com/mcp").
    #[serde(default)]
    pub url: String,
    /// HTTP headers for authentication (HTTP transport).
    /// e.g. `{"Authorization": "Bearer ${TOKEN}"}`
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// Environment variables injected into the MCP server process (stdio transport).
    /// e.g. `{"BRAVE_API_KEY": "xxx"}` — mirrors Claude Code's `"env"` field.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Whether this server is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_transport() -> String {
    "stdio".into()
}

/// Wrapper to parse only the `[[mcp_servers]]` table from config.toml.
/// Other keys are ignored (serde default), so this is forward-compatible.
#[derive(Deserialize)]
struct McpServersFile {
    #[serde(default)]
    mcp_servers: Vec<McpServerConfig>,
}

/// Parse `[[mcp_servers]]` entries from `data/config.toml` into the config.
/// Mirrors `load_llm_from_file` (app_state) but for MCP server definitions.
/// Best-effort: malformed entries or a missing file leave `mcp_servers` as-is.
fn load_mcp_servers(config: &mut AppConfig) {
    let path = config.data_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    match toml::from_str::<McpServersFile>(&content) {
        Ok(parsed) => {
            if !parsed.mcp_servers.is_empty() {
                tracing::info!(
                    count = parsed.mcp_servers.len(),
                    "MCP servers loaded from config.toml"
                );
            }
            config.mcp_servers = parsed.mcp_servers;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse mcp_servers from config.toml — skipping");
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_permission_level_str() -> String {
    "semi_auto".into()
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    3000
}
fn default_max_context_tokens() -> usize {
    100_000
}
fn default_summarize_threshold() -> f32 {
    0.7
}

impl Default for AppConfig {
    fn default() -> Self {
        let data_dir = resolve_data_dir();
        let config_dir = data_dir.join("config");
        Self {
            data_dir,
            config_dir,
            server_host: default_host(),
            server_port: default_port(),
            llm_providers: Vec::new(),
            default_provider: "anthropic".into(),
            database_path: None,
            max_context_tokens: default_max_context_tokens(),
            summarize_threshold: default_summarize_threshold(),
            default_permission_level: default_permission_level_str(),
            mcp_servers: Vec::new(),
            workspace_dir: None,
            embedding_model: None,
        }
    }
}

impl AppConfig {
    /// Load configuration from environment variables and defaults.
    ///
    /// Priority: env vars > defaults.
    pub fn load() -> Result<Self, crate::EverEvoError> {
        // ── .env loading (manual, zero-dependency) ────────────────────
        // Load .env from the working directory so users can put API keys
        // and other secrets in environment variables instead of TOML.
        // Silently skips missing .env files (they're optional).
        // Format: KEY=VALUE, # comments, blank lines, no quoting needed.
        if let Ok(content) = std::fs::read_to_string(".env") {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some(eq) = trimmed.find('=') {
                    let key = trimmed[..eq].trim();
                    let value = trimmed[eq + 1..].trim();
                    // Only set if not already in env (env overrides .env)
                    if std::env::var(key).is_err() && !key.is_empty() && !value.is_empty() {
                        std::env::set_var(key, value);
                    }
                }
            }
            tracing::info!(".env file loaded");
        }

        let mut config = Self::default();

        // Server settings from env
        if let Ok(host) = std::env::var("EVEREVO_HOST") {
            config.server_host = host;
        }
        if let Ok(port) = std::env::var("EVEREVO_PORT") {
            config.server_port = port
                .parse()
                .map_err(|_| crate::EverEvoError::Config(format!("Invalid port: {port}")))?;
        }

        // LLM providers from env
        if let Some(anthropic) = LlmProviderConfig::from_env_anthropic() {
            config.llm_providers.push(anthropic);
        }
        if let Some(openai) = LlmProviderConfig::from_env_openai() {
            config.llm_providers.push(openai);
        }
        if let Some(ollama) = LlmProviderConfig::from_env_ollama() {
            config.llm_providers.push(ollama);
        }

        if let Ok(default) = std::env::var("EVEREVO_DEFAULT_PROVIDER") {
            config.default_provider = default;
        }

        // ── Plaintext API key warning ─────────────────────────────────
        // All providers loaded from env are safe (no TOML plaintext).
        // Warn only about keys hardcoded in data/config/config.toml.
        // This runs BEFORE the TOML file is read — providers from config.toml
        // are loaded separately by the server; the warning here covers the
        // env-based fast path. The real TOML warning fires at server startup
        // in app_state.rs when loading LLM providers from the config file.
        if config.llm_providers.iter().any(|p| {
            p.api_key.as_ref().is_some_and(|k| !k.is_empty())
                && std::env::var("ANTHROPIC_API_KEY").is_err()
                && std::env::var("OPENAI_API_KEY").is_err()
        }) {
            tracing::warn!(
                "API keys loaded from environment variables — plaintext TOML keys are NOT used. \
                 For better security, store keys in .env (loaded automatically) or environment variables, \
                 not in data/config/config.toml."
            );
        }

        // LLM providers optional — server starts without them for bootstrap UI

        // Ensure data directory and subdirectories exist
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            crate::EverEvoError::Config(format!(
                "Failed to create data directory {}: {e}",
                config.data_dir.display()
            ))
        })?;

        for sub in &[
            "db",
            "memory/vector",
            "memory/graph",
            "sandbox",
            "memory/diary",
            "memory/facts",
            "memory/.dreams",
            "memory/wiki",
            "workflows",
        ] {
            std::fs::create_dir_all(config.data_dir.join(sub)).map_err(|e| {
                crate::EverEvoError::Config(format!(
                    "Failed to create subdir {}: {e}",
                    config.data_dir.join(sub).display()
                ))
            })?;
        }

        // Ensure config_dir is in sync with data_dir (in case it changed).
        config.config_dir = config.data_dir.join("config");

        // Create config directory and write defaults.toml on first run.
        std::fs::create_dir_all(&config.config_dir).map_err(|e| {
            crate::EverEvoError::Config(format!(
                "Failed to create config directory {}: {e}",
                config.config_dir.display()
            ))
        })?;

        let config_toml = config.config_dir.join("config.toml");
        if !config_toml.exists() {
            std::fs::write(&config_toml, crate::config_center::defaults_toml_content()).map_err(
                |e| {
                    crate::EverEvoError::Config(format!(
                        "Failed to write default config to {}: {e}",
                        config_toml.display()
                    ))
                },
            )?;
        }

        // Load MCP server configs from data/config.toml [[mcp_servers]].
        // Without this, mcp_servers stays empty (AppConfig::load is env-only).
        load_mcp_servers(&mut config);

        Ok(config)
    }

    /// Resolve the database path — absolute, or relative to data_dir.
    pub fn database_path(&self) -> PathBuf {
        match &self.database_path {
            Some(p) if PathBuf::from(p).is_absolute() => PathBuf::from(p),
            Some(p) => self.data_dir.join(p),
            None => self.data_dir.join("db").join("everevo.db"),
        }
    }
}

// Credential configuration removed — sandbox now inherits host git config directly.
// Global ~/.gitconfig and ~/.ssh are used as-is, eliminating ambiguity.

// ── Data Directory Resolution ──────────────────────────────────────────

/// Resolve the runtime data directory.
///
/// 1. `EVEREVO_DATA_DIR` env var (optional override)
/// 2. `./data/` relative to CWD (default, dev & prod both use project root)
pub fn resolve_data_dir() -> PathBuf {
    // Priority 1: explicit environment variable
    if let Ok(dir) = std::env::var("EVEREVO_DATA_DIR") {
        return PathBuf::from(dir);
    }

    // Default: ./data/ relative to current working directory
    std::env::current_dir()
        .map(|d| d.join("data"))
        .unwrap_or_else(|_| {
            // CWD unavailable — fall back to executable-relative path
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("data")))
                .unwrap_or_else(|| PathBuf::from("./data"))
        })
}

// ── LLM Provider Helpers ───────────────────────────────────────────────

impl LlmProviderConfig {
    pub fn from_env_anthropic() -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        Some(Self {
            kind: crate::types::LlmProviderKind::Anthropic,
            api_key: Some(api_key),
            model: std::env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-5-20250929".into()),
            base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
            max_tokens: std::env::var("ANTHROPIC_MAX_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok()),
        })
    }

    pub fn from_env_openai() -> Option<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").ok()?;
        Some(Self {
            kind: crate::types::LlmProviderKind::OpenAI,
            api_key: Some(api_key),
            model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into()),
            base_url: std::env::var("OPENAI_BASE_URL").ok(),
            max_tokens: std::env::var("OPENAI_MAX_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok()),
        })
    }

    pub fn from_env_ollama() -> Option<Self> {
        let base_url = std::env::var("OLLAMA_BASE_URL").ok()?;
        Some(Self {
            kind: crate::types::LlmProviderKind::Ollama,
            api_key: None,
            model: std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.1".into()),
            base_url: Some(base_url),
            max_tokens: std::env::var("OLLAMA_MAX_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_data_dir_defaults_to_cwd_data() {
        let dir = resolve_data_dir();
        assert!(dir.ends_with("data"), "Expected .../data, got: {dir:?}");
    }

    #[test]
    fn test_default_config_has_data_dir() {
        let config = AppConfig::default();
        assert!(!config.data_dir.as_os_str().is_empty());
    }

    #[test]
    fn test_database_path_default() {
        let config = AppConfig::default();
        let db_path = config.database_path();
        assert!(db_path.ends_with("everevo.db"), "Got: {db_path:?}");
    }
}
