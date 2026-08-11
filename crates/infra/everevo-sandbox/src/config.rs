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

    /// Populate `injected_paths` with detected host runtime directories
    /// (Python, Node, etc.) so the sandbox has access to real interpreters
    /// rather than Windows App Execution Alias stubs.
    pub fn detect_runtimes(&mut self) {
        // Python: look for Windows py launcher and common install locations
        if let Ok(py_home) = std::env::var("PYTHON_HOME") {
            self.injected_paths.push(PathBuf::from(&py_home));
        }
        // Common Python install directories (Windows)
        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let python_programs = PathBuf::from(&local_appdata)
            .join("Programs")
            .join("Python");
        if python_programs.exists() {
            // Scan for Python3* directories
            if let Ok(entries) = std::fs::read_dir(&python_programs) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("Python3"))
                    {
                        self.injected_paths.push(p.clone());
                        // Also add Scripts/ subdirectory for pip-installed tools
                        let scripts = p.join("Scripts");
                        if scripts.exists() {
                            self.injected_paths.push(scripts);
                        }
                    }
                }
            }
        }
        // Node.js common path
        let node_global = PathBuf::from(&local_appdata)
            .join("Volta") // Volta is a popular Node version manager
            .join("bin");
        if node_global.exists() {
            self.injected_paths.push(node_global);
        }
        // npm global prefix
        if let Ok(prefix) = std::process::Command::new("npm")
            .args(["prefix", "-g"])
            .output()
        {
            let path = String::from_utf8_lossy(&prefix.stdout).trim().to_string();
            if !path.is_empty() {
                self.injected_paths.push(PathBuf::from(&path));
            }
        }
    }
}

/// Human-readable explanation for common Windows exit codes.
/// Attached to stderr when a command exits with a non-zero code that
/// has a known cause.
pub fn exit_code_explanation(exit_code: i32, _command: &str) -> Option<&'static str> {
    match exit_code {
        49 => Some(
            "Exit code 49 on Windows usually means the command launched an App \
             Execution Alias (a Microsoft Store stub). This happens when a \
             program name (python3, node, etc.) resolves to the \
             %LOCALAPPDATA%\\Microsoft\\WindowsApps\\ stub instead of the real \
             executable. The sandbox now filters WindowsApps from PATH — try \
             using 'python' or 'py -3' instead of 'python3'.",
        ),
        -1 => Some(
            "Process was killed (timeout or signal). Try a simpler command \
             or increase the timeout.",
        ),
        _ => None,
    }
}
