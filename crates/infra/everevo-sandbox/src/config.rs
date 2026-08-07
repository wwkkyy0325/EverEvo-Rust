//! Sandbox configuration.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Root for per-session sandbox directories.
    pub sandbox_root: PathBuf,
    /// Default execution timeout in seconds.
    pub default_timeout_secs: u64,
    /// Maximum execution timeout in seconds.
    pub max_timeout_secs: u64,
    /// Default memory limit in MB (None = no limit).
    pub default_memory_mb: Option<u64>,
    /// Whether to prefer WSL when available.
    pub prefer_wsl: bool,
    /// Whether to use Job Objects on Windows.
    pub use_job_objects: bool,
    /// PATH entries injected into all sandboxed processes.
    pub injected_paths: Vec<PathBuf>,
    /// Environment variables injected into all sandboxed processes.
    pub injected_env: Vec<(String, String)>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            sandbox_root: PathBuf::from("./data/sandbox"),
            default_timeout_secs: 30,
            max_timeout_secs: 300,
            default_memory_mb: Some(512),
            prefer_wsl: true,
            use_job_objects: true,
            injected_paths: vec![],
            // Force UTF-8 everywhere — prevents Windows GBK encoding crashes.
            // PYTHONIOENCODING: Python stdout/stderr encoding
            // PYTHONUTF8: PEP 540 — Python uses UTF-8 mode
            // LANG: fallback for other tools
            injected_env: vec![
                ("PYTHONIOENCODING".into(), "utf-8".into()),
                ("PYTHONUTF8".into(), "1".into()),
                ("LANG".into(), "en_US.UTF-8".into()),
                ("LC_ALL".into(), "en_US.UTF-8".into()),
            ],
        }
    }
}

impl SandboxConfig {
    pub fn with_injected_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.injected_paths = paths;
        self
    }
}
