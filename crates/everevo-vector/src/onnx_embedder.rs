//! ONNX embedding model — fastembed with pre-flight ORT version check.
//!
//! Only compiled when the `onnx` feature is enabled. Falls back to
//! DummyEmbedder at runtime when ORT is too old or model files are missing.

use std::path::{Path, PathBuf};
use everevo_core::EverEvoError;
use super::embedding::EmbeddingModel;

// ── ORT DLL path configuration (always compiled) ──────────────────────

/// Point ORT to our vendored ONNX Runtime DLL by setting `ORT_DYLIB_PATH`.
///
/// Call this early in `main()` before any ONNX Runtime code runs.
/// Idempotent — safe to call multiple times.
///
/// For Tauri/WebView2 processes, the Tauri entry point also preloads the
/// DLL via `LoadLibraryExW` to win the race against `System32`. This
/// function only sets the env var (no unsafe code needed).
pub fn configure_ort_dylib(data_dir: &Path) {
    let ort_dir = data_dir.join("runtime").join("onnxruntime");
    let dll = ort_dir.join("lib").join("onnxruntime.dll");
    if !dll.exists() {
        tracing::warn!("onnxruntime.dll not found at {} — falling back to system", dll.display());
        return;
    }
    if std::env::var("ORT_DYLIB_PATH").unwrap_or_default().is_empty() {
        std::env::set_var("ORT_DYLIB_PATH", &dll);
        tracing::info!(path=%dll.display(), "ORT_DYLIB_PATH set");
    }
}

// ── No-ONNX fallback (feature off) ──────────────────────────────────

#[cfg(not(feature = "onnx"))]
pub struct OnnxEmbedder;

#[cfg(not(feature = "onnx"))]
impl OnnxEmbedder {
    pub fn new(_key: &str, _dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> { Ok(Self) }
    pub fn model_key(&self) -> &str { "none" }
    pub fn is_loaded(&self) -> bool { false }
}

#[cfg(not(feature = "onnx"))]
impl EmbeddingModel for OnnxEmbedder {
    fn encode(&self, _text: &str) -> Result<Vec<f32>, EverEvoError> { Ok(vec![0.0_f32; 384]) }
    fn dimension(&self) -> usize { 384 }
}

#[cfg(not(feature = "onnx"))]
pub struct OnnxCheckResult { pub model_key: String, pub loaded: bool, pub smoke_passed: bool, pub error: Option<String> }

#[cfg(not(feature = "onnx"))]
pub fn check_onnx_model(key: &str, _dir: &std::path::Path) -> Option<OnnxCheckResult> {
    Some(OnnxCheckResult { model_key: key.into(), loaded: false, smoke_passed: false, error: Some("onnx feature off".into()) })
}

// ── Real ONNX implementation (feature on) ───────────────────────────

#[cfg(feature = "onnx")]
use std::sync::{Mutex, OnceLock};

#[cfg(feature = "onnx")]
static ORT_VERSION_OK: OnceLock<bool> = OnceLock::new();

#[cfg(feature = "onnx")]
fn is_ort_compatible(data_dir: &std::path::Path) -> bool {
    *ORT_VERSION_OK.get_or_init(|| {
        let ort_dir = data_dir.join("runtime").join("onnxruntime");
        let vf = find_file_in(&ort_dir, "VERSION_NUMBER");
        match vf.and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(v) => {
                let parts: Vec<u32> = v.trim().split('.').filter_map(|s| s.parse().ok()).collect();
                // ort v2.0.0-rc.12 + fastembed v5 request `api-24`, which maps to
                // ONNX Runtime ≥ v1.24.x. The VERSION_NUMBER is written by bootstrap.
                let ok = parts.len() >= 2 && parts[0] >= 1 && (parts[0] > 1 || parts[1] >= 24);
                tracing::info!(version=%v.trim(), compatible=ok, "ONNX Runtime version check");
                ok
            }
            None => { tracing::warn!("Cannot read ORT version — ONNX disabled"); false }
        }
    })
}

#[cfg(feature = "onnx")]
fn find_file_in(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let d = dir.join(name); if d.exists() { return Some(d); }
    std::fs::read_dir(dir).ok()?.flatten()
        .filter_map(|e| if e.path().is_dir() { Some(e.path().join(name)) } else { None })
        .find(|p| p.exists())
}

#[cfg(feature = "onnx")]
pub struct OnnxEmbedder { dim: usize, model_key: String, inner: Option<Mutex<fastembed::TextEmbedding>> }

#[cfg(feature = "onnx")]
impl OnnxEmbedder {
    pub fn new(model_key: &str, models_dir: impl Into<PathBuf>) -> Result<Self, EverEvoError> {
        let models_dir: PathBuf = models_dir.into();
        let model_dir = models_dir.join(model_key);
        let data_dir = models_dir.parent().unwrap_or(&models_dir).parent().unwrap_or(std::path::Path::new("data"));
        let inner = if is_ort_compatible(data_dir) {
            match load_fastembed(model_key, &model_dir) {
                Ok(e) => { tracing::info!(key=%model_key, "ONNX model loaded"); Some(Mutex::new(e)) }
                Err(e) => { tracing::warn!(key=%model_key, err=%e, "fastembed init failed"); None }
            }
        } else { None };
        Ok(Self { dim: 384, model_key: model_key.into(), inner })
    }
    pub fn model_key(&self) -> &str { &self.model_key }
    pub fn is_loaded(&self) -> bool { self.inner.is_some() }
}

#[cfg(feature = "onnx")]
fn load_fastembed(_key: &str, model_dir: &std::path::Path) -> Result<fastembed::TextEmbedding, String> {
    if !model_dir.exists() { return Err("dir missing".into()); }
    let onnx = std::fs::read(model_dir.join("model_quantized.onnx")).map_err(|e| format!("onnx: {e}"))?;
    let tok = std::fs::read(model_dir.join("tokenizer.json")).map_err(|e| format!("tok: {e}"))?;
    let cfg = std::fs::read(model_dir.join("config.json")).unwrap_or_default();
    let stm = std::fs::read(model_dir.join("special_tokens_map.json")).unwrap_or_default();
    let tcfg = std::fs::read(model_dir.join("tokenizer_config.json")).unwrap_or_default();
    use fastembed::TextEmbedding;
    let m = fastembed::UserDefinedEmbeddingModel {
        onnx_file: onnx, external_initializers: vec![],
        tokenizer_files: fastembed::TokenizerFiles { tokenizer_file: tok, config_file: cfg, special_tokens_map_file: stm, tokenizer_config_file: tcfg },
        pooling: Some(fastembed::Pooling::Mean), quantization: fastembed::QuantizationMode::Dynamic, output_key: None,
    };
    TextEmbedding::try_new_from_user_defined(m, fastembed::InitOptionsUserDefined::new()).map_err(|e| format!("{e}"))
}

#[cfg(feature = "onnx")]
impl EmbeddingModel for OnnxEmbedder {
    fn encode(&self, text: &str) -> Result<Vec<f32>, EverEvoError> {
        let Some(ref i) = self.inner else { return Ok(vec![0.0_f32; self.dim]) };
        let mut e = i.lock().map_err(|e| EverEvoError::Internal(format!("lock: {e}")))?;
        let r = e.embed(vec![text], None).map_err(|e| EverEvoError::Vector(format!("encode: {e}")))?;
        Ok(r.into_iter().next().unwrap_or_else(|| vec![0.0_f32; self.dim]))
    }
    fn dimension(&self) -> usize { self.dim }
}

#[cfg(feature = "onnx")]
pub struct OnnxCheckResult { pub model_key: String, pub loaded: bool, pub smoke_passed: bool, pub error: Option<String> }

#[cfg(feature = "onnx")]
pub fn check_onnx_model(model_key: &str, models_dir: &std::path::Path) -> Option<OnnxCheckResult> {
    let data_dir = models_dir.parent().unwrap_or(models_dir);
    if !is_ort_compatible(data_dir) {
        return Some(OnnxCheckResult { model_key: model_key.into(), loaded: false, smoke_passed: false, error: Some("ORT too old".into()) });
    }
    match OnnxEmbedder::new(model_key, models_dir) {
        Ok(e) => {
            let ok = e.is_loaded() && e.encode("test").map(|v| !v.iter().all(|x| *x == 0.0)).unwrap_or(false);
            Some(OnnxCheckResult { model_key: model_key.into(), loaded: e.is_loaded(), smoke_passed: ok, error: if ok { None } else { Some("zero vector".into()) } })
        }
        Err(e) => Some(OnnxCheckResult { model_key: model_key.into(), loaded: false, smoke_passed: false, error: Some(e.to_string()) }),
    }
}
