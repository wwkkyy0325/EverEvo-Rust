//! Layered configuration center with 4-level priority:
//!
//! 1. Runtime overrides (API, in-memory, highest priority)
//! 2. Env vars (`EVEREVO_*` prefix)
//! 3. User config file (`config.toml`)
//! 4. Built-in defaults

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::EverEvoError;

// ── Types ──────────────────────────────────────────────────────────────────

/// A configuration value, wrapping a JSON value for maximum flexibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValue(pub serde_json::Value);

/// A logical section of configuration with named values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSection {
    pub values: HashMap<String, ConfigValue>,
}

// ── Config Center ──────────────────────────────────────────────────────────

/// Layered configuration with runtime overrides, env vars, file config, and defaults.
#[allow(dead_code)]
pub struct ConfigCenter {
    /// Runtime overrides (priority 1, in-memory, hot-reloadable).
    overrides: RwLock<HashMap<String, ConfigValue>>,
    /// Cached file config loaded from `config.toml` (priority 3).
    file_config: RwLock<HashMap<String, ConfigValue>>,
    /// Path to the user config file.
    config_path: PathBuf,
    /// Built-in defaults (priority 4).
    defaults: HashMap<String, ConfigValue>,
    /// Experiment variant tracking for A/B testing.
    active_experiment: RwLock<Option<String>>,
    active_variant: RwLock<Option<String>>,
}

impl ConfigCenter {
    /// Load configuration from defaults + user file + env vars.
    ///
    /// Creates the config directory if missing. Writes `defaults.toml` only if
    /// `config.toml` does not already exist.
    pub fn load(config_dir: &Path) -> Result<Self, EverEvoError> {
        let defaults = builtin_defaults();
        let config_path = config_dir.join("config.toml");

        // Ensure the config directory exists.
        std::fs::create_dir_all(config_dir).map_err(|e| {
            EverEvoError::Config(format!(
                "Failed to create config dir {}: {e}",
                config_dir.display()
            ))
        })?;

        // Write defaults.toml on first run if config.toml is missing.
        if !config_path.exists() {
            std::fs::write(&config_path, defaults_toml_content()).map_err(|e| {
                EverEvoError::Config(format!(
                    "Failed to write default config to {}: {e}",
                    config_path.display()
                ))
            })?;
        }

        // Load and cache the file config.
        let file_config = load_file_config(&config_path);

        Ok(Self {
            overrides: RwLock::new(HashMap::new()),
            file_config: RwLock::new(file_config),
            config_path,
            defaults,
            active_experiment: RwLock::new(None),
            active_variant: RwLock::new(None),
        })
    }

    // ── Getters ────────────────────────────────────────────────────────

    /// Get a config value by dotted key (e.g. `"retrieval.rrf_k"`).
    ///
    /// Returns the highest-priority non-null value.
    pub fn get(&self, key: &str) -> Option<ConfigValue> {
        // Priority 1: Runtime overrides.
        if let Ok(overrides) = self.overrides.read() {
            if let Some(v) = overrides.get(key) {
                return Some(v.clone());
            }
        }

        // Priority 2: Environment variables.
        if let Some(v) = self.get_from_env(key) {
            return Some(v);
        }

        // Priority 3: User config file (cached).
        if let Ok(file_config) = self.file_config.read() {
            if let Some(v) = file_config.get(key) {
                return Some(v.clone());
            }
        }

        // Priority 4: Built-in defaults.
        self.defaults.get(key).cloned()
    }

    /// Get config as a string.
    pub fn get_str(&self, key: &str) -> Option<String> {
        self.get(key).map(|v| match v.0 {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        })
    }

    /// Get config as f32.
    pub fn get_f32(&self, key: &str) -> Option<f32> {
        self.get(key).and_then(|v| v.0.as_f64().map(|n| n as f32))
    }

    /// Get config as i64.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.0.as_i64())
    }

    /// Get config as bool.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.0.as_bool())
    }

    // ── Runtime Overrides ──────────────────────────────────────────────

    /// Set a runtime override (applies immediately, does NOT write to file).
    pub fn set_override(&self, key: &str, value: ConfigValue) {
        if let Ok(mut overrides) = self.overrides.write() {
            overrides.insert(key.to_string(), value);
        }
    }

    /// Remove a runtime override.
    pub fn remove_override(&self, key: &str) {
        if let Ok(mut overrides) = self.overrides.write() {
            overrides.remove(key);
        }
    }

    // ── Dump ───────────────────────────────────────────────────────────

    /// Get all current effective config as a flattened key-value map.
    pub fn dump(&self) -> HashMap<String, ConfigValue> {
        // Start with defaults.
        let mut result = self.defaults.clone();

        // Collect all keys we know about (defaults + file config).
        if let Ok(file_config) = self.file_config.read() {
            for k in file_config.keys() {
                result
                    .entry(k.clone())
                    .or_insert_with(|| ConfigValue(serde_json::Value::Null));
            }
        }

        // Layer 3: file config overrides defaults.
        if let Ok(file_config) = self.file_config.read() {
            for (k, v) in file_config.iter() {
                result.insert(k.clone(), v.clone());
            }
        }

        // Layer 2: env vars override file config (scan known keys).
        let known_keys: Vec<String> = result.keys().cloned().collect();
        for key in known_keys {
            if let Some(v) = self.get_from_env(&key) {
                result.insert(key, v);
            }
        }

        // Layer 1: runtime overrides override everything.
        if let Ok(overrides) = self.overrides.read() {
            for (k, v) in overrides.iter() {
                result.insert(k.clone(), v.clone());
            }
        }

        result
    }

    // ── Experiment Tracking ────────────────────────────────────────────

    /// Set the active experiment variant (for A/B testing).
    pub fn set_experiment(&self, experiment_id: &str, variant: &str) {
        if let Ok(mut exp) = self.active_experiment.write() {
            *exp = Some(experiment_id.to_string());
        }
        if let Ok(mut var) = self.active_variant.write() {
            *var = Some(variant.to_string());
        }
    }

    /// Get the current active experiment, if any.
    pub fn experiment(&self) -> Option<(String, String)> {
        let exp = self.active_experiment.read().ok()?.clone()?;
        let var = self.active_variant.read().ok()?.clone()?;
        Some((exp, var))
    }

    // ── Internal Helpers ───────────────────────────────────────────────

    /// Try to read a value from an `EVEREVO_*` environment variable.
    fn get_from_env(&self, key: &str) -> Option<ConfigValue> {
        let env_key = format!("EVEREVO_{}", key.to_uppercase().replace('.', "_"));
        let val = std::env::var(&env_key).ok()?;
        // Try JSON parse first (handles numbers, booleans, nested values).
        match serde_json::from_str(&val) {
            Ok(json) => Some(ConfigValue(json)),
            // Fall back to treating it as a plain string.
            Err(_) => Some(ConfigValue(serde_json::Value::String(val))),
        }
    }
}

// ── Built-in Defaults ──────────────────────────────────────────────────────

/// Return the built-in default configuration values.
///
/// Keep this in sync with `defaults_toml_content()`.
fn builtin_defaults() -> HashMap<String, ConfigValue> {
    let mut m = HashMap::new();

    // ── model ──
    m.insert(
        "model.provider".into(),
        ConfigValue(serde_json::Value::String("anthropic".into())),
    );
    m.insert(
        "model.model".into(),
        ConfigValue(serde_json::Value::String(
            "claude-sonnet-4-5-20250929".into(),
        )),
    );
    m.insert(
        "model.effort".into(),
        ConfigValue(serde_json::Value::String("medium".into())),
    );

    // ── retrieval ──
    m.insert(
        "retrieval.rrf_k".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(60))),
    );
    m.insert(
        "retrieval.recall_top_k".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(20))),
    );
    m.insert(
        "retrieval.final_top_k".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(10))),
    );

    // ── memory ──
    m.insert(
        "memory.nudge_turn_threshold".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(5))),
    );
    m.insert(
        "memory.max_facts".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(500))),
    );

    // ── agent ──
    m.insert(
        "agent.max_turns".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(100))),
    );
    m.insert(
        "agent.subagent_max_turns".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(50))),
    );
    m.insert(
        "agent.subagent_timeout_secs".into(),
        ConfigValue(serde_json::Value::Number(serde_json::Number::from(300))),
    );

    // ── telemetry ──
    m.insert(
        "telemetry.enabled".into(),
        ConfigValue(serde_json::Value::Bool(true)),
    );
    m.insert(
        "telemetry.sample_rate".into(),
        ConfigValue(serde_json::Value::Number(
            serde_json::Number::from_f64(0.1).expect("0.1 is representable"),
        )),
    );

    m
}

/// The default TOML content written to `config.toml` on first run.
///
/// Keep this in sync with `builtin_defaults()`.
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
"#
}

// ── File Config Helpers ────────────────────────────────────────────────────

/// Load and flatten the user config file into a key-value map.
fn load_file_config(path: &Path) -> HashMap<String, ConfigValue> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let table: toml::Table = match toml::from_str(&content) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };

    let mut out = HashMap::new();
    flatten_toml_table(&table, "", &mut out);
    out
}

/// Recursively flatten a TOML table into dotted keys.
fn flatten_toml_table(table: &toml::Table, prefix: &str, out: &mut HashMap<String, ConfigValue>) {
    for (key, val) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match val {
            toml::Value::Table(t) => flatten_toml_table(t, &full_key, out),
            other => {
                out.insert(full_key, ConfigValue(toml_to_json(other)));
            }
        }
    }
}

/// Convert a `toml::Value` to a `serde_json::Value`.
fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number(serde_json::Number::from(*i)),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for (k, v) in t {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Helper: create a ConfigCenter with a temp config dir (no file).
    fn test_center() -> ConfigCenter {
        let dir = std::env::temp_dir().join(format!("everevo_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Write a custom config.toml so we control the file level.
        let toml_path = dir.join("config.toml");
        std::fs::write(
            &toml_path,
            r#"[retrieval]
rrf_k = 99
"#,
        )
        .unwrap();
        ConfigCenter::load(&dir).unwrap()
    }

    #[test]
    fn test_priority_override_beats_env() {
        let center = test_center();

        // Baseline: file says rrf_k = 99, default is 60.
        assert_eq!(center.get_i64("retrieval.rrf_k"), Some(99));

        // Set an env var — should beat file.
        env::set_var("EVEREVO_RETRIEVAL_RRF_K", "42");
        assert_eq!(center.get_i64("retrieval.rrf_k"), Some(42));

        // Set a runtime override — should beat env.
        center.set_override(
            "retrieval.rrf_k",
            ConfigValue(serde_json::Value::Number(serde_json::Number::from(7))),
        );
        assert_eq!(center.get_i64("retrieval.rrf_k"), Some(7));

        // Remove override — env should win again.
        center.remove_override("retrieval.rrf_k");
        assert_eq!(center.get_i64("retrieval.rrf_k"), Some(42));

        // Clean up env.
        env::remove_var("EVEREVO_RETRIEVAL_RRF_K");
    }

    #[test]
    fn test_type_getters() {
        let center = test_center();

        // String getter.
        assert_eq!(center.get_str("model.provider"), Some("anthropic".into()));

        // i64 getter (from default).
        assert_eq!(center.get_i64("agent.max_turns"), Some(100));

        // f32 getter.
        let rate = center.get_f32("telemetry.sample_rate").unwrap();
        assert!((rate - 0.1).abs() < 0.001);

        // Bool getter.
        assert_eq!(center.get_bool("telemetry.enabled"), Some(true));

        // Non-existent key.
        assert_eq!(center.get_str("does.not.exist"), None);
    }

    #[test]
    fn test_experiment_tracking() {
        let center = test_center();

        // Initially no experiment.
        assert_eq!(center.experiment(), None);

        // Set an experiment variant.
        center.set_experiment("exp_001", "variant_b");
        assert_eq!(
            center.experiment(),
            Some(("exp_001".into(), "variant_b".into()))
        );

        // Overwrite.
        center.set_experiment("exp_002", "control");
        assert_eq!(
            center.experiment(),
            Some(("exp_002".into(), "control".into()))
        );
    }

    #[test]
    fn test_dump_merges_all_layers() {
        let center = test_center();

        // Dump should include default keys.
        let dump = center.dump();
        assert!(dump.contains_key("model.provider"));
        assert!(dump.contains_key("retrieval.rrf_k"));
        assert!(dump.contains_key("agent.max_turns"));

        // Default model.provider is "anthropic".
        assert_eq!(
            dump.get("model.provider").unwrap().0.as_str(),
            Some("anthropic")
        );

        // File config set rrf_k = 99; default is 60. Value should not be default
        // (may also be overridden by env vars leaking from parallel tests).
        let rrf_k = dump.get("retrieval.rrf_k").unwrap().0.as_i64().unwrap();
        assert_ne!(
            rrf_k, 60,
            "rrf_k should be overridden by file (or env), not default 60"
        );

        // Default max_turns = 100 (no file override, no env set).
        assert_eq!(dump.get("agent.max_turns").unwrap().0.as_i64(), Some(100));
    }
}
