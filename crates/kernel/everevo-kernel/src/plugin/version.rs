//! Filesystem-based plugin version management.
//!
//! ## Directory layout
//!
//! ```text
//! data/plugins/{plugin_id}/
//!   versions/
//!     v1.0.0/
//!       plugin.exe
//!       plugin.toml       ← metadata
//!       checksum.sha256   ← binary integrity check
//!     v1.0.1/
//!       ...
//!   registry.toml          ← active versions + metrics
//! ```
//!
//! ## Design
//!
//! - No database — filesystem is the source of truth
//! - SHA256 checksums prevent corrupted binaries from being loaded
//! - Symlinks for stable/canary resolution (zero-read overhead)
//! - registry.toml holds version config + metrics (human-readable)

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Error type ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum VersionError {
    Io(std::io::Error),
    Toml(toml_error::Error),
    NotFound { plugin_id: String, version: String },
    ChecksumMismatch { expected: String, actual: String },
    InvalidVersion(String),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Toml(e) => write!(f, "TOML error: {e}"),
            Self::NotFound { plugin_id, version } => {
                write!(f, "Plugin '{plugin_id}' version '{version}' not found")
            }
            Self::ChecksumMismatch { expected, actual } => {
                write!(f, "Checksum mismatch: expected {expected}, got {actual}")
            }
            Self::InvalidVersion(v) => write!(f, "Invalid version format: {v}"),
        }
    }
}

impl std::error::Error for VersionError {}

impl From<std::io::Error> for VersionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// Need a simple TOML error wrapper
mod toml_error {
    #[derive(Debug)]
    pub enum Error {
        Ser(String),
        De(String),
    }
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Ser(s) => write!(f, "TOML serialize: {s}"),
                Self::De(s) => write!(f, "TOML deserialize: {s}"),
            }
        }
    }
    impl std::error::Error for Error {}
}

// ── Plugin Metrics ──────────────────────────────────────────────────────

/// Per-version accumulated metrics for canary decision-making.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginMetrics {
    pub success_count: u64,
    pub error_count: u64,
    pub total_count: u64,
    pub total_latency_ms: u64,
    pub crash_count: u64,
    /// ISO-8601 timestamp of when metrics were last reset
    #[serde(default)]
    pub last_reset: String,
}

impl PluginMetrics {
    pub fn success_rate(&self) -> f64 {
        if self.total_count == 0 {
            return 1.0;
        }
        self.success_count as f64 / self.total_count as f64
    }

    pub fn avg_latency_ms(&self) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        self.total_latency_ms as f64 / self.total_count as f64
    }

    pub fn record(&mut self, success: bool, latency_ms: u64) {
        self.total_count += 1;
        self.total_latency_ms += latency_ms;
        if success {
            self.success_count += 1;
        } else {
            self.error_count += 1;
        }
    }

    pub fn record_crash(&mut self) {
        self.crash_count += 1;
    }
}

// ── Plugin Config ───────────────────────────────────────────────────────

/// Per-plugin configuration stored in registry.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Current stable version (all traffic by default).
    pub stable: String,
    /// Optional canary version (receives canary_pct of traffic).
    #[serde(default)]
    pub canary: Option<String>,
    /// Percentage of traffic routed to canary (0.0 .. 1.0).
    #[serde(default)]
    pub canary_pct: f64,
    /// Whether auto-promote is enabled.
    #[serde(default = "default_true")]
    pub auto_promote: bool,
    /// Whether auto-rollback is enabled.
    #[serde(default = "default_true")]
    pub auto_rollback: bool,
    /// Minimum minutes to observe canary before auto-promote.
    #[serde(default = "default_observe_minutes")]
    pub promote_min_minutes: u64,
    /// Per-version metrics.
    #[serde(default)]
    pub metrics: HashMap<String, PluginMetrics>,
}

fn default_true() -> bool { true }
fn default_observe_minutes() -> u64 { 30 }

// ── Version Store ───────────────────────────────────────────────────────

/// Filesystem-backed plugin version manager.
///
/// Thread-safe: all methods take `&self` and do blocking I/O.
/// For async contexts, wrap in `tokio::task::spawn_blocking`.
pub struct VersionStore {
    plugins_dir: PathBuf,
}

impl VersionStore {
    /// Open (or create) the plugin registry directory.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, VersionError> {
        let plugins_dir: PathBuf = dir.into();
        std::fs::create_dir_all(&plugins_dir)?;
        Ok(Self { plugins_dir })
    }

    /// Get the executable path for a specific plugin version.
    pub fn exe_path(&self, plugin_id: &str, version: &str) -> PathBuf {
        self.plugins_dir
            .join(plugin_id)
            .join("versions")
            .join(version)
            .join("plugin.exe")
    }

    /// Get the config file path for a plugin.
    fn config_path(&self, plugin_id: &str) -> PathBuf {
        self.plugins_dir.join(plugin_id).join("registry.toml")
    }

    /// Load plugin config from registry.toml (or create default).
    pub fn load_config(&self, plugin_id: &str) -> Result<PluginConfig, VersionError> {
        let path = self.config_path(plugin_id);
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let cfg: PluginConfig =
                toml::from_str(&content).map_err(|e| VersionError::Toml(toml_error::Error::De(e.to_string())))?;
            Ok(cfg)
        } else {
            Err(VersionError::NotFound {
                plugin_id: plugin_id.into(),
                version: "any".into(),
            })
        }
    }

    /// Save plugin config to registry.toml.
    pub fn save_config(
        &self,
        plugin_id: &str,
        config: &PluginConfig,
    ) -> Result<(), VersionError> {
        let path = self.config_path(plugin_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(config)
            .map_err(|e| VersionError::Toml(toml_error::Error::Ser(e.to_string())))?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Deterministically resolve which version to use for a session.
    ///
    /// Uses session_id hash for consistent routing — the same session
    /// always gets the same version.
    pub fn resolve(&self, config: &PluginConfig, session_id: Uuid) -> String {
        match &config.canary {
            Some(v) if config.canary_pct > 0.0 => {
                let bucket = (session_id.as_u128() % 10000) as f64 / 10000.0;
                if bucket < config.canary_pct {
                    return v.clone();
                }
            }
            _ => {}
        }
        config.stable.clone()
    }

    /// Stage a new plugin version (copy binary + generate checksum).
    pub fn stage(
        &self,
        plugin_id: &str,
        version: &str,
        exe_path: &Path,
    ) -> Result<(), VersionError> {
        let target_dir = self
            .plugins_dir
            .join(plugin_id)
            .join("versions")
            .join(version);
        std::fs::create_dir_all(&target_dir)?;

        let target_exe = target_dir.join("plugin.exe");
        std::fs::copy(exe_path, &target_exe)?;

        // Generate checksum
        let hash = sha256_file(&target_exe)?;
        std::fs::write(target_dir.join("checksum.sha256"), &hash)?;

        tracing::info!(%plugin_id, %version, %hash, "Plugin version staged");
        Ok(())
    }

    /// Set canary version and traffic percentage.
    pub fn set_canary(
        &self,
        plugin_id: &str,
        version: &str,
        pct: f64,
    ) -> Result<(), VersionError> {
        let mut config = self.load_config(plugin_id)?;
        config.canary = Some(version.to_string());
        config.canary_pct = pct.clamp(0.0, 1.0);
        config.metrics.entry(version.to_string()).or_default();
        self.save_config(plugin_id, &config)?;
        tracing::info!(%plugin_id, %version, %pct, "Canary activated");
        Ok(())
    }

    /// Promote canary to stable.
    pub fn promote(&self, plugin_id: &str) -> Result<(), VersionError> {
        let mut config = self.load_config(plugin_id)?;
        if let Some(v) = config.canary.take() {
            config.stable = v;
            config.canary_pct = 0.0;
            self.save_config(plugin_id, &config)?;
            tracing::info!(%plugin_id, stable = config.stable, "Canary promoted to stable");
        }
        Ok(())
    }

    /// Rollback: remove canary, keep stable unchanged.
    pub fn rollback(&self, plugin_id: &str) -> Result<(), VersionError> {
        let mut config = self.load_config(plugin_id)?;
        let old = config.canary.take();
        config.canary_pct = 0.0;
        self.save_config(plugin_id, &config)?;
        tracing::warn!(%plugin_id, ?old, stable = config.stable, "Canary rolled back");
        Ok(())
    }

    /// Record a tool call result for metrics.
    pub fn record_call(
        &self,
        plugin_id: &str,
        version: &str,
        success: bool,
        latency_ms: u64,
    ) -> Result<(), VersionError> {
        let mut config = self.load_config(plugin_id)?;
        config
            .metrics
            .entry(version.to_string())
            .or_default()
            .record(success, latency_ms);
        self.save_config(plugin_id, &config)?;
        Ok(())
    }

    /// Record a crash for a plugin version.
    pub fn record_crash(
        &self,
        plugin_id: &str,
        version: &str,
    ) -> Result<(), VersionError> {
        let mut config = self.load_config(plugin_id)?;
        config
            .metrics
            .entry(version.to_string())
            .or_default()
            .record_crash();
        self.save_config(plugin_id, &config)?;
        Ok(())
    }

    /// Verify a plugin binary's checksum.
    pub fn verify_checksum(
        &self,
        plugin_id: &str,
        version: &str,
    ) -> Result<(), VersionError> {
        let exe = self.exe_path(plugin_id, version);
        let checksum_path = exe.parent().unwrap().join("checksum.sha256");

        if !checksum_path.exists() {
            return Ok(()); // no checksum to verify
        }

        let expected = std::fs::read_to_string(&checksum_path)?.trim().to_string();
        let actual = sha256_file(&exe)?;

        if expected != actual {
            return Err(VersionError::ChecksumMismatch { expected, actual });
        }
        Ok(())
    }

    /// List all staged versions for a plugin.
    pub fn list_versions(&self, plugin_id: &str) -> Result<Vec<String>, VersionError> {
        let versions_dir = self.plugins_dir.join(plugin_id).join("versions");
        if !versions_dir.exists() {
            return Ok(Vec::new());
        }
        let mut versions = Vec::new();
        for entry in std::fs::read_dir(&versions_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                versions.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        versions.sort();
        Ok(versions)
    }
}

// ── SHA256 helper ───────────────────────────────────────────────────────

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_version_store_stage_and_resolve() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open(dir.path()).unwrap();

        // Create a fake plugin exe
        let fake_exe = dir.path().join("fake.exe");
        std::fs::write(&fake_exe, b"fake binary").unwrap();

        // Stage it
        store
            .stage("test-plugin", "v1.0.0", &fake_exe)
            .unwrap();

        // Verify checksum was generated
        let checksum_path = store
            .exe_path("test-plugin", "v1.0.0")
            .parent()
            .unwrap()
            .join("checksum.sha256");
        assert!(checksum_path.exists());

        // Verify checksum passes
        store
            .verify_checksum("test-plugin", "v1.0.0")
            .unwrap();
    }

    #[test]
    fn test_canary_routing_deterministic() {
        let config = PluginConfig {
            stable: "v1.0.0".into(),
            canary: Some("v1.0.1".into()),
            canary_pct: 0.5,
            auto_promote: true,
            auto_rollback: true,
            promote_min_minutes: 30,
            metrics: HashMap::new(),
        };

        // Same session always routes to same version
        let sid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let store = VersionStore::open(".").unwrap(); // dummy store, resolve doesn't need it
        let v1 = store.resolve(&config, sid);
        let v2 = store.resolve(&config, sid);
        assert_eq!(v1, v2);

        // Different sessions may route differently but shouldn't panic
        let _v3 = store.resolve(&config, Uuid::new_v4());
    }

    #[test]
    fn test_canary_pct_zero_always_stable() {
        let config = PluginConfig {
            stable: "v1.0.0".into(),
            canary: Some("v1.0.1".into()),
            canary_pct: 0.0,
            auto_promote: true,
            auto_rollback: true,
            promote_min_minutes: 30,
            metrics: HashMap::new(),
        };

        let store = VersionStore::open(".").unwrap();
        for _ in 0..100 {
            assert_eq!(
                store.resolve(&config, Uuid::new_v4()),
                "v1.0.0"
            );
        }
    }

    #[test]
    fn test_metrics_recording() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open(dir.path()).unwrap();

        // Create initial config
        let config = PluginConfig {
            stable: "v1.0.0".into(),
            canary: None,
            canary_pct: 0.0,
            auto_promote: true,
            auto_rollback: true,
            promote_min_minutes: 30,
            metrics: HashMap::new(),
        };
        store.save_config("test", &config).unwrap();

        // Record some calls
        store.record_call("test", "v1.0.0", true, 50).unwrap();
        store.record_call("test", "v1.0.0", true, 60).unwrap();
        store.record_call("test", "v1.0.0", false, 100).unwrap();

        // Verify metrics
        let config = store.load_config("test").unwrap();
        let m = config.metrics.get("v1.0.0").unwrap();
        assert_eq!(m.total_count, 3);
        assert_eq!(m.success_count, 2);
        assert_eq!(m.error_count, 1);
        assert_eq!(m.total_latency_ms, 210);
    }

    #[test]
    fn test_promote_and_rollback() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open(dir.path()).unwrap();

        let config = PluginConfig {
            stable: "v1.0.0".into(),
            canary: Some("v1.0.1".into()),
            canary_pct: 0.1,
            auto_promote: true,
            auto_rollback: true,
            promote_min_minutes: 30,
            metrics: HashMap::new(),
        };
        store.save_config("test", &config).unwrap();

        // Promote
        store.promote("test").unwrap();
        let promoted = store.load_config("test").unwrap();
        assert_eq!(promoted.stable, "v1.0.1");
        assert!(promoted.canary.is_none());

        // Set up canary again then rollback
        store.set_canary("test", "v1.0.2", 0.1).unwrap();
        store.rollback("test").unwrap();
        let rolled = store.load_config("test").unwrap();
        assert_eq!(rolled.stable, "v1.0.1"); // stable unchanged
        assert!(rolled.canary.is_none());
    }

    #[test]
    fn test_checksum_verification_fails_for_corrupt_binary() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open(dir.path()).unwrap();

        let fake_exe = dir.path().join("fake.exe");
        std::fs::write(&fake_exe, b"original").unwrap();
        store.stage("test", "v1.0.0", &fake_exe).unwrap();

        // Corrupt the binary
        std::fs::write(store.exe_path("test", "v1.0.0"), b"corrupted").unwrap();

        // Verification should fail
        let result = store.verify_checksum("test", "v1.0.0");
        assert!(result.is_err());
        match result.unwrap_err() {
            VersionError::ChecksumMismatch { .. } => {} // expected
            e => panic!("expected ChecksumMismatch, got {e:?}"),
        }
    }
}
