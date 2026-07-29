//! Model registry — auto-discovers ONNX embedding models under `data/models/`.
//!
//! ## Discovery
//!
//! Scans `data/models/*/` for directories containing `model_quantized.onnx` +
//! `config.json`. Extracts `hidden_size` (BERT-family) or `dim` (other archs)
//! from config.json to determine embedding dimension.
//!
//! ## Aliasing
//!
//! Collections are named `{namespace}-{dim}` (e.g. `memory-384`). The registry
//! maintains logical aliases (e.g. "memory" → "memory-384") so consumers always
//! access the right collection for the active model dimension.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use everevo_core::EverEvoError;

/// Metadata for a discovered model.
#[derive(Debug, Clone)]
pub struct ModelMeta {
    /// Directory name under models/ (e.g. "all-MiniLM-L6-v2").
    pub name: String,
    /// Human-readable model identifier from config.json.
    pub display_name: String,
    /// Embedding dimension.
    pub dim: usize,
    /// Model directory path.
    pub path: PathBuf,
    /// Whether this model is currently active.
    pub active: bool,
}

/// Manages ONNX embedding model discovery and activation.
pub struct ModelRegistry {
    /// All discovered models, keyed by directory name.
    models: HashMap<String, ModelMeta>,
    /// Currently active model name.
    active: String,
}

impl ModelRegistry {
    /// Scan `models_dir` for ONNX models and set the active model.
    ///
    /// `preferred` — if Some, try to activate this model. If None or not found,
    /// falls back to the first discovered model.
    pub fn discover(models_dir: impl Into<PathBuf>, preferred: Option<&str>) -> Result<Self, EverEvoError> {
        let models_dir: PathBuf = models_dir.into();
        let mut models = HashMap::new();

        if models_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&models_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() { continue; }
                    if let Some(meta) = Self::try_read_model(&path) {
                        models.insert(meta.name.clone(), meta);
                    }
                }
            }
        }

        if models.is_empty() {
            return Err(EverEvoError::Config(
                format!("No ONNX models found under {}", models_dir.display())
            ));
        }

        // Determine active model.
        let active = preferred
            .and_then(|p| models.contains_key(p).then_some(p.to_string()))
            .or_else(|| models.keys().next().cloned())
            .unwrap_or_default();

        if let Some(m) = models.get_mut(&active) {
            m.active = true;
        }

        tracing::info!(
            count = models.len(),
            active = %active,
            "Model registry initialized"
        );

        Ok(Self { models, active })
    }

    /// The currently active model.
    pub fn active(&self) -> &ModelMeta {
        &self.models[&self.active]
    }

    /// Active model's embedding dimension.
    pub fn active_dim(&self) -> usize {
        self.active().dim
    }

    /// List all discovered models.
    pub fn list(&self) -> Vec<&ModelMeta> {
        let mut v: Vec<&ModelMeta> = self.models.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Activate a model by name. Returns the new active model metadata.
    pub fn activate(&mut self, name: &str) -> Result<ModelMeta, EverEvoError> {
        if !self.models.contains_key(name) {
            return Err(EverEvoError::InvalidInput(format!("Model '{}' not found", name)));
        }
        // Deactivate previous.
        let old_name = self.active.clone();
        if let Some(old) = self.models.get_mut(&old_name) {
            old.active = false;
        }
        // Activate new.
        let model = self.models.get_mut(name).unwrap();
        model.active = true;
        self.active = name.to_string();
        let meta = model.clone();
        tracing::info!(model = %name, dim = meta.dim, "Model activated");
        Ok(meta)
    }

    /// Collection path for a logical namespace with the active model's dim.
    pub fn collection_path(&self, base_dir: &Path, namespace: &str) -> PathBuf {
        base_dir.join(format!("{}-{}", namespace, self.active_dim()))
    }

    // ── Internal ───────────────────────────────────────────────────────

    fn try_read_model(dir: &Path) -> Option<ModelMeta> {
        let onnx = dir.join("model_quantized.onnx");
        if !onnx.exists() { return None; }

        let config_path = dir.join("config.json");
        let dim = Self::read_dim(&config_path)?;

        let name = dir.file_name()?.to_string_lossy().to_string();
        let display_name = if config_path.exists() {
            Self::read_display_name(&config_path).unwrap_or_else(|| name.clone())
        } else {
            name.clone()
        };

        tracing::debug!(%name, dim, "Discovered model");
        Some(ModelMeta {
            name,
            display_name,
            dim,
            path: dir.to_path_buf(),
            active: false,
        })
    }

    fn read_dim(config_path: &Path) -> Option<usize> {
        let content = std::fs::read_to_string(config_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;

        // BERT-family models: hidden_size
        if let Some(d) = json.get("hidden_size").and_then(|v| v.as_u64()) {
            return Some(d as usize);
        }
        // Other architectures: dim, d_model
        if let Some(d) = json.get("dim").and_then(|v| v.as_u64()) {
            return Some(d as usize);
        }
        if let Some(d) = json.get("d_model").and_then(|v| v.as_u64()) {
            return Some(d as usize);
        }
        None
    }

    fn read_display_name(config_path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(config_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;
        json.get("_name_or_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_discover_bert_model() {
        let dir = TempDir::new().unwrap();
        let model_dir = dir.path().join("test-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("model_quantized.onnx"), b"fake").unwrap();
        std::fs::write(model_dir.join("config.json"), r#"{"hidden_size": 768, "_name_or_path": "test/model"}"#).unwrap();

        let reg = ModelRegistry::discover(dir.path(), None).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.active_dim(), 768);
        assert_eq!(reg.active().display_name, "test/model");
    }

    #[test]
    fn test_discover_multi_model() {
        let dir = TempDir::new().unwrap();
        for (name, dim) in &[("a", 384), ("b", 768)] {
            let md = dir.path().join(name);
            std::fs::create_dir_all(&md).unwrap();
            std::fs::write(md.join("model_quantized.onnx"), b"fake").unwrap();
            std::fs::write(md.join("config.json"), format!(r#"{{"hidden_size": {}}}"#, dim)).unwrap();
        }

        let reg = ModelRegistry::discover(dir.path(), Some("b")).unwrap();
        assert_eq!(reg.list().len(), 2);
        assert_eq!(reg.active_dim(), 768);
    }

    #[test]
    fn test_activate_switch() {
        let dir = TempDir::new().unwrap();
        for name in &["a", "b"] {
            let md = dir.path().join(name);
            std::fs::create_dir_all(&md).unwrap();
            std::fs::write(md.join("model_quantized.onnx"), b"fake").unwrap();
            std::fs::write(md.join("config.json"), r#"{"hidden_size": 384}"#).unwrap();
        }

        let mut reg = ModelRegistry::discover(dir.path(), Some("a")).unwrap();
        assert_eq!(reg.active().name, "a");
        reg.activate("b").unwrap();
        assert_eq!(reg.active().name, "b");
    }

    #[test]
    fn test_no_onnx_skipped() {
        let dir = TempDir::new().unwrap();
        let md = dir.path().join("no-model");
        std::fs::create_dir_all(&md).unwrap();
        std::fs::write(md.join("config.json"), r#"{"hidden_size": 384}"#).unwrap();
        // No model_quantized.onnx → should be skipped.

        let reg = ModelRegistry::discover(dir.path(), None);
        assert!(reg.is_err());
    }
}
