//! Built-in default configuration — used for first-run `config.toml` generation.
//!
//! `ConfigCenter` struct (formerly here) was removed as dead code — it was marked
//! `#[allow(dead_code)]` and never instantiated in production. Only the default
//! TOML template is kept. For configuration loading, see `crate::config::AppConfig`.

/// The default TOML content written to `config.toml` on first run.
///
/// Keep this in sync with `AppConfig` defaults in `config.rs`.
pub fn defaults_toml_content() -> &'static str {
    r#"# EverEvo Configuration — generated on first run.
# Edit this file to customize. Runtime overrides and EVEREVO_* env vars
# take precedence over values set here.

[model]
# LLM provider: "anthropic", "openai", or "ollama"
provider = "anthropic"
# Default model name
model = "claude-sonnet-4-5-20250929"
# Reasoning effort: "low", "medium", or "high"
effort = "medium"

[retrieval]
# Reciprocal Rank Fusion constant
rrf_k = 60
# Number of candidates to recall from vector + graph search
recall_top_k = 20
# Final count after reranking
final_top_k = 10

[memory]
# Turn threshold before nudging the agent to reflect
nudge_turn_threshold = 5
# Maximum number of facts stored in working memory
max_facts = 500

[agent]
# Maximum turns for the main agent loop
max_turns = 100
# Maximum turns for subagent calls
subagent_max_turns = 50
# Timeout in seconds for subagent execution
subagent_timeout_secs = 300

[telemetry]
# Whether telemetry collection is enabled
enabled = true
# Sampling rate: 1.0 = all events, 0.1 = 10%
sample_rate = 0.1

# ── MCP servers ──────────────────────────────────────────────────────
# Browser automation via Playwright (recommended). Uncomment to enable
# real-browser tools: browser_navigate / browser_click / browser_evaluate /
# browser_snapshot (accessibility tree) / browser_screenshot (vision).
# Requires: Node.js is auto-bootstrapped. First browser use needs a one-time
# `npx playwright install chromium` (runnable from EverEvo's shell tool).
# [[mcp_servers]]
# name = "playwright"
# transport = "stdio"
# command = "npx"
# args = ["-y", "@playwright/mcp@latest"]
"#
}
