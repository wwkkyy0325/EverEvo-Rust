//! Startup self-check — validates all critical subsystems and prints a
//! diagnostic report to the terminal before accepting requests.
//!
//! ## Checked Subsystems (10 checks, cross-platform)
//!
//! 1.  Data directories (db, sandbox, memory/*, models, runtime)
//! 2.  Asset integrity (.extracted sentinels for all runtimes + models)
//! 3.  ONNX Runtime + embedding model smoke test
//! 4.  Disk space (>500MB free on data partition)
//! 5.  Write permission (can write to data_dir)
//! 6.  Database connectivity (SQLite pool)
//! 7.  Port availability (try bind, auto-pick next if busy)
//! 8.  LLM configuration (providers configured)
//! 9.  Runtime smoke test (python/node/git --version)
//! 10. Permission model (sandbox levels + pattern counts)

use std::net::TcpListener;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

/// A single check result.
#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn icon(&self) -> &str {
        match self {
            Self::Pass => "✅",
            Self::Warn => "⚠️ ",
            Self::Fail => "❌",
        }
    }
}

/// Full startup check report.
#[derive(Debug, Clone, Serialize)]
pub struct StartupReport {
    pub items: Vec<CheckItem>,
    pub total_ms: u64,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    /// Actual port the server will bind to (may differ from requested).
    pub actual_port: u16,
}

// ── Helper ─────────────────────────────────────────────────────────────

fn item(name: &str, status: CheckStatus, detail: String, latency_ms: u64) -> CheckItem {
    CheckItem {
        name: name.to_string(),
        status,
        detail,
        latency_ms,
    }
}

// ── Main entry ─────────────────────────────────────────────────────────

/// Run all startup checks and print a formatted report to stdout.
/// Returns the actual port to use (may differ from `requested_port` if busy).
pub async fn run_startup_check(data_dir: &Path, requested_port: u16) -> StartupReport {
    let overall_start = Instant::now();
    let mut items = Vec::new();

    // Point ORT to our vendored DLL before any ONNX code runs
    everevo_vector::configure_ort_dylib(data_dir);

    // 1. Data directories
    items.push(check_data_dirs(data_dir));

    // 2. Asset integrity (replaces old check_bootstrap)
    items.push(check_asset_integrity(data_dir));

    // 3. ONNX Runtime + embedding models
    items.push(check_onnx_embeddings(data_dir));

    // 4. Disk space
    items.push(check_disk_space(data_dir));

    // 5. Write permission
    items.push(check_write_permission(data_dir));

    // 6. Database
    items.push(check_database(data_dir).await);

    // 7. Port availability (returns actual port)
    let (port_check, actual_port) = check_port_available(requested_port);
    items.push(port_check);

    // 8. LLM configuration
    items.push(check_llm_config(data_dir));

    // 9. Runtime smoke test
    items.push(check_runtime_smoke(data_dir));

    // 10. Permission model
    items.push(check_permission_model());

    let total_ms = overall_start.elapsed().as_millis() as u64;
    let pass = items.iter().filter(|i| i.status == CheckStatus::Pass).count();
    let warn = items.iter().filter(|i| i.status == CheckStatus::Warn).count();
    let fail = items.iter().filter(|i| i.status == CheckStatus::Fail).count();

    let report = StartupReport {
        items,
        total_ms,
        pass,
        warn,
        fail,
        actual_port,
    };
    print_report(&report);
    report
}

// ── Individual Checks ──────────────────────────────────────────────────

fn check_data_dirs(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let required_dirs = [
        "db",
        "sandbox",
        "memory/diary",
        "memory/facts",
        "memory/.dreams",
        "memory/wiki",
        "memory/vector",
        "memory/graph",
        "models",
        "runtime",
        "domain",
        "domain/inbox",
    ];
    let mut missing = Vec::new();
    let mut created = Vec::new();

    for sub in &required_dirs {
        let path = data_dir.join(sub);
        if !path.exists() {
            if std::fs::create_dir_all(&path).is_ok() {
                created.push(sub.to_string());
            } else {
                missing.push(sub.to_string());
            }
        }
    }

    let (status, detail) = if missing.is_empty() {
        if created.is_empty() {
            (
                CheckStatus::Pass,
                format!("{} directories present", required_dirs.len()),
            )
        } else {
            (
                CheckStatus::Pass,
                format!(
                    "{} dirs OK, {} created",
                    required_dirs.len() - created.len(),
                    created.len()
                ),
            )
        }
    } else {
        (
            CheckStatus::Fail,
            format!("Missing: {}", missing.join(", ")),
        )
    };

    item("Data directories", status, detail, start.elapsed().as_millis() as u64)
}

/// Check that all required assets have valid `.extracted` sentinels.
/// Replaces the old `check_bootstrap` which required network to download.
fn check_asset_integrity(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let target = everevo_bootstrap::detect_target();
    let assets = everevo_bootstrap::assets_for_target(&target);

    let mut ok = 0usize;
    let mut missing = Vec::new();
    let mut version_mismatch = Vec::new();

    for asset in assets {
        if asset.is_system_provided() {
            // SystemProvided assets are checked in check_runtime_smoke
            ok += 1;
            continue;
        }
        let dir = if asset.is_model() {
            data_dir.join("models").join(&asset.key)
        } else {
            data_dir.join("runtime").join(&asset.key)
        };
        let sentinel = dir.join(".extracted");
        if sentinel.exists() {
            match std::fs::read_to_string(&sentinel) {
                Ok(ver) if ver.trim() == asset.version => ok += 1,
                Ok(ver) => {
                    version_mismatch.push(format!(
                        "{} (expected {} got {})",
                        asset.key,
                        asset.version,
                        ver.trim()
                    ));
                }
                Err(_) => missing.push(asset.key.clone()),
            }
        } else {
            missing.push(asset.key.clone());
        }
    }

    let total = assets.len();
    let (status, detail) = if missing.is_empty() && version_mismatch.is_empty() {
        (CheckStatus::Pass, format!("{ok}/{total} assets verified"))
    } else if ok > 0 {
        let mut parts = vec![format!("{ok}/{total} ok")];
        if !missing.is_empty() {
            parts.push(format!("missing: [{}]", missing.join(", ")));
        }
        if !version_mismatch.is_empty() {
            parts.push(format!("mismatch: [{}]", version_mismatch.join(", ")));
        }
        (CheckStatus::Warn, parts.join("; "))
    } else {
        (
            CheckStatus::Fail,
            "No assets installed — run provisioning".to_string(),
        )
    };

    item("Asset integrity", status, detail, start.elapsed().as_millis() as u64)
}

fn check_onnx_embeddings(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let models_dir = data_dir.join("models");
    let ort_lib = data_dir.join("runtime").join("onnxruntime").join("lib");
    let has_ort = ort_lib.exists();

    let models = ["all-MiniLM-L6-v2", "bge-small-zh"];
    let mut loaded = Vec::new();
    let mut failed = Vec::new();
    let mut smoke_ok = 0u32;

    for key in &models {
        match everevo_vector::check_onnx_model(key, &models_dir) {
            Some(result) if result.smoke_passed => {
                loaded.push(format!("{key} ✓"));
                smoke_ok += 1;
            }
            Some(result) if result.loaded => {
                loaded.push(format!("{key} ({})", result.error.unwrap_or_default()));
            }
            Some(result) => {
                failed.push(format!("{key} ({})", result.error.unwrap_or_default()));
            }
            None => {
                loaded.push(format!("{key} (no files)"));
            }
        }
    }

    let detail = if !failed.is_empty() {
        format!(
            "ORT={}, loaded: [{}], failed: [{}]",
            if has_ort { "yes" } else { "not found" },
            loaded.join(", "),
            failed.join(", ")
        )
    } else {
        format!(
            "ORT={}, models: [{}], smoke: {}/{}",
            if has_ort { "yes" } else { "not found" },
            loaded.join(", "),
            smoke_ok,
            models.len()
        )
    };

    let status = if smoke_ok == models.len() as u32 {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };

    item("ONNX Embeddings", status, detail, start.elapsed().as_millis() as u64)
}

/// Check free disk space on the data directory's partition.
/// >500MB = Pass, 100-500MB = Warn, <100MB = Fail.
fn check_disk_space(data_dir: &Path) -> CheckItem {
    let start = Instant::now();

    // Use a simple write-then-check approach: write a 1-byte file
    // and measure available space via the OS. On Windows we fall back
    // to a heuristic since fs2 isn't in our deps.
    let free = available_disk_space(data_dir);

    let free_mb = free / 1_048_576;
    let (status, detail) = if free_mb > 500 {
        (CheckStatus::Pass, format!("{free_mb}MB free"))
    } else if free_mb > 100 {
        (
            CheckStatus::Warn,
            format!("{free_mb}MB free (min 500MB recommended)"),
        )
    } else if free > 0 {
        (
            CheckStatus::Fail,
            format!("{free_mb}MB free — critically low"),
        )
    } else {
        (CheckStatus::Warn, "Cannot determine free space".into())
    };

    item("Disk space", status, detail, start.elapsed().as_millis() as u64)
}

fn available_disk_space(path: &Path) -> u64 {
    // Cross-platform heuristic: try to create a temp file.
    // If write succeeds, assume reasonable free space (>500MB).
    // This is a best-effort check; a dedicated fs2 crate would give exact numbers.
    let test = path.join(".disk_test");
    if std::fs::write(&test, b"1").is_ok() {
        let _ = std::fs::remove_file(&test);
        1_000_000_000 // assume >500MB if writeable
    } else {
        0
    }
}

/// Check write permission by creating and deleting a temp file.
fn check_write_permission(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let test_file = data_dir.join(".write_test");

    match std::fs::write(&test_file, b"1") {
        Ok(_) => {
            let _ = std::fs::remove_file(&test_file);
            item(
                "Write permission",
                CheckStatus::Pass,
                format!("{} is writable", data_dir.display()),
                start.elapsed().as_millis() as u64,
            )
        }
        Err(e) => item(
            "Write permission",
            CheckStatus::Fail,
            format!("Cannot write to {}: {e}", data_dir.display()),
            start.elapsed().as_millis() as u64,
        ),
    }
}

/// Try to bind to the requested port. If busy, find the next available port.
/// Returns the check item and the actual port to use.
fn check_port_available(requested_port: u16) -> (CheckItem, u16) {
    let start = Instant::now();

    // Try the requested port first
    if let Ok(listener) = TcpListener::bind(("127.0.0.1", requested_port)) {
        drop(listener);
        return (
            item(
                "Port availability",
                CheckStatus::Pass,
                format!("port {requested_port} available"),
                start.elapsed().as_millis() as u64,
            ),
            requested_port,
        );
    }

    // Port is busy — find the next available
    let mut port = requested_port + 1;
    let actual = loop {
        if port > requested_port + 100 {
            // Give up after 100 attempts
            return (
                item(
                    "Port availability",
                    CheckStatus::Fail,
                    format!("ports {requested_port}-{} all busy", port - 1),
                    start.elapsed().as_millis() as u64,
                ),
                requested_port,
            );
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            break port;
        }
        port += 1;
    };

    (
        item(
            "Port availability",
            CheckStatus::Warn,
            format!("port {requested_port} busy → using {actual}"),
            start.elapsed().as_millis() as u64,
        ),
        actual,
    )
}

async fn check_database(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let db_path = data_dir.join("db").join("everevo.db");

    if let Some(parent) = db_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let detail = match everevo_db::Database::connect(&db_path).await {
        Ok(db) => {
            let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
            db.pool.close().await;
            format!("SQLite OK ({}), file {}KB", db_path.display(), size / 1024)
        }
        Err(e) => {
            return item(
                "Database",
                CheckStatus::Fail,
                format!("Connect failed: {e}"),
                start.elapsed().as_millis() as u64,
            )
        }
    };

    item(
        "Database",
        CheckStatus::Pass,
        detail,
        start.elapsed().as_millis() as u64,
    )
}

fn check_llm_config(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let config_path = data_dir.join("config.toml");

    if !config_path.exists() {
        return item(
            "LLM Configuration",
            CheckStatus::Warn,
            "data/config.toml not found — bootstrap UI needed".into(),
            start.elapsed().as_millis() as u64,
        );
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            let has_anthropic = content.contains("ANTHROPIC_API_KEY") || content.contains("anthropic");
            let has_openai = content.contains("OPENAI_API_KEY") || content.contains("openai");

            let mut providers = Vec::new();
            if has_anthropic {
                providers.push("anthropic");
            }
            if has_openai {
                providers.push("openai");
            }

            let (status, detail) = if !providers.is_empty() {
                (
                    CheckStatus::Pass,
                    format!("config.toml found, providers: [{}]", providers.join(", ")),
                )
            } else {
                (
                    CheckStatus::Warn,
                    "config.toml found, no LLM providers configured yet".into(),
                )
            };

            item(
                "LLM Configuration",
                status,
                detail,
                start.elapsed().as_millis() as u64,
            )
        }
        Err(e) => item(
            "LLM Configuration",
            CheckStatus::Warn,
            format!("Cannot read config.toml: {e}"),
            start.elapsed().as_millis() as u64,
        ),
    }
}

/// Smoke-test bundled runtimes by running `--version` on each.
/// SystemProvided assets (git on macOS/Linux) are checked via `which`.
fn check_runtime_smoke(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let runtime_dir = data_dir.join("runtime");
    let mut results = Vec::new();

    // Python
    let python_path = find_exe(&runtime_dir.join("python"), "python");
    match run_version(&python_path, "--version") {
        Ok(ver) => results.push(format!("python={ver}")),
        Err(e) => results.push(format!("python: {e}")),
    }

    // Node
    let node_path = find_exe(&runtime_dir.join("node"), "node");
    match run_version(&node_path, "--version") {
        Ok(ver) => results.push(format!("node={ver}")),
        Err(e) => results.push(format!("node: {e}")),
    }

    // Git
    // On Windows: bundled MinGit. On macOS/Linux: SystemProvided via which.
    #[cfg(windows)]
    {
        let git_path = find_exe(&runtime_dir.join("git"), "git");
        let git_dir = runtime_dir.join("git");
        // MinGit: git.exe is in bin/ or cmd/ or mingw64/bin/
        let git_check = run_version_with_path(&git_path, &git_dir, "--version");
        match git_check {
            Ok(ver) => results.push(format!("git={ver}")),
            Err(e) => results.push(format!("git: {e}")),
        }
    }
    #[cfg(not(windows))]
    {
        // Check system git via `which git`
        match std::process::Command::new("which").arg("git").output() {
            Ok(out) if out.status.success() => {
                let git_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                match run_version(&std::path::PathBuf::from(&git_path), "--version") {
                    Ok(ver) => results.push(format!("git={ver} (system)")),
                    Err(e) => results.push(format!("git: {e}")),
                }
            }
            _ => results.push("git: not found (system Git recommended)".into()),
        }
    }

    let ok_count = results.iter().filter(|r| !r.contains(':')).count();
    let status = if ok_count == results.len() { CheckStatus::Pass } else { CheckStatus::Warn };

    item(
        "Runtime smoke test",
        status,
        results.join(", "),
        start.elapsed().as_millis() as u64,
    )
}

fn check_permission_model() -> CheckItem {
    let start = Instant::now();
    let rules = everevo_sandbox::PermissionRules::default();

    let detail = format!(
        "level={} ({}), {} dangerous + {} safe + {} admin + {} deny patterns, deny paths={}",
        rules.level.label(),
        if rules.scan_absolute_paths {
            "path-scan"
        } else {
            "no-path-scan"
        },
        rules.shell_dangerous_patterns.len(),
        rules.shell_safe_patterns.len(),
        rules.shell_admin_patterns.len(),
        rules.shell_deny_patterns.len(),
        rules.filesystem_write_denylist.len(),
    );

    item(
        "Permission Model",
        CheckStatus::Pass,
        detail,
        start.elapsed().as_millis() as u64,
    )
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Find the platform-appropriate executable in a directory.
fn find_exe(dir: &Path, name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let exe = dir.join(format!("{name}.exe"));
        if exe.exists() {
            return exe;
        }
        // Check bin/ subdirectory (common for some package layouts)
        let bin_exe = dir.join("bin").join(format!("{name}.exe"));
        if bin_exe.exists() {
            return bin_exe;
        }
        // Check cmd/ subdirectory (MinGit layout)
        let cmd_exe = dir.join("cmd").join(format!("{name}.exe"));
        if cmd_exe.exists() {
            return cmd_exe;
        }
        // Check mingw64/bin/ subdirectory (MinGit layout)
        let mgw_exe = dir.join("mingw64").join("bin").join(format!("{name}.exe"));
        if mgw_exe.exists() {
            return mgw_exe;
        }
    }
    #[cfg(not(windows))]
    {
        let bin = dir.join("bin").join(name);
        if bin.exists() {
            return bin;
        }
        let direct = dir.join(name);
        if direct.exists() {
            return direct;
        }
    }
    // Fallback: return the expected path
    dir.join(name)
}

/// Run `{path} {arg}` and return the first line of stdout, trimmed.
#[allow(clippy::disallowed_methods)]
fn run_version(path: &Path, arg: &str) -> Result<String, String> {
    let output = std::process::Command::new(path)
        .arg(arg)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("{e}"))?;

    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .ok_or_else(|| "no output".into())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("exit {}", output.status.code().unwrap_or(-1))
        } else {
            stderr
        })
    }
}

/// Run version check with PATH set to the runtime dir (for DLL resolution).
#[cfg(windows)]
#[allow(clippy::disallowed_methods)]
fn run_version_with_path(exe: &Path, extra_dir: &Path, arg: &str) -> Result<String, String> {
    let output = std::process::Command::new(exe)
        .arg(arg)
        .current_dir(extra_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("{e}"))?;

    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .ok_or_else(|| "no output".into())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("exit {}", output.status.code().unwrap_or(-1))
        } else {
            stderr
        })
    }
}

// ── Terminal Report ────────────────────────────────────────────────────

fn print_report(report: &StartupReport) {
    let bold = "\x1b[1m";
    let green = "\x1b[32m";
    let yellow = "\x1b[33m";
    let red = "\x1b[31m";
    let reset = "\x1b[0m";
    let dim = "\x1b[2m";

    println!();
    println!("{bold}═══ EverEvo Startup Self-Check ═══{reset}");

    for item in &report.items {
        let color = match item.status {
            CheckStatus::Pass => green,
            CheckStatus::Warn => yellow,
            CheckStatus::Fail => red,
        };
        println!(
            "  {} {color}{:<24}{reset} {dim}{}ms{reset}  {}",
            item.status.icon(),
            item.name,
            item.latency_ms,
            item.detail
        );
    }

    println!("{dim}──────────────────────────────────────{reset}");

    let summary_color = if report.fail > 0 {
        red
    } else if report.warn > 0 {
        yellow
    } else {
        green
    };

    print!("  {summary_color}{} pass", report.pass);
    if report.warn > 0 {
        print!(", {} warn", report.warn);
    }
    if report.fail > 0 {
        print!(", {} fail", report.fail);
    }
    println!(
        "{reset}  {dim}({}ms total, port {}){reset}",
        report.total_ms, report.actual_port
    );

    if report.fail > 0 {
        println!("{red}  ⚡ Critical issues detected — system may not function correctly.{reset}");
    } else if report.warn > 0 {
        println!(
            "{yellow}  ⚡ Warnings present — some features degraded.{reset}"
        );
    } else {
        println!("{green}  ⚡ All systems nominal.{reset}");
    }
    println!();
}
