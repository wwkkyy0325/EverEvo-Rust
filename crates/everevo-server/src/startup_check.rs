//! Startup self-check — validates all critical subsystems and prints a
//! diagnostic report to the terminal before accepting requests.
//!
//! ## Checked Subsystems
//!
//! 1. Data directories (db, vectors, graph, sandbox, memory/*, models, runtime)
//! 2. Bootstrap assets (runtimes + embedding models)
//! 3. ONNX Runtime + embedding models (loads + inference smoke test)
//! 4. Database connectivity (SQLite pool + migrations)
//! 5. Configuration (LLM providers configured)
//! 6. Permission model (sandbox levels + dangerous pattern count)

use std::path::Path;
use std::time::Instant;

/// A single check result.
#[derive(Debug, Clone)]
pub struct CheckItem {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
pub struct StartupReport {
    pub items: Vec<CheckItem>,
    pub total_ms: u64,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

/// Run all startup checks and print a formatted report to stdout.
pub async fn run_startup_check(data_dir: &Path) -> StartupReport {
    let overall_start = Instant::now();
    let mut items = Vec::new();

    // Point ORT to our vendored DLL before any ONNX code runs;
    // Windows has a stale onnxruntime.dll v1.17.1 in System32.
    everevo_vector::configure_ort_dylib(data_dir);

    // ── 1. Data directories ──────────────────────────────
    items.push(check_data_dirs(data_dir));

    // ── 2. Bootstrap assets ──────────────────────────────
    items.push(check_bootstrap(data_dir).await);

    // ── 3. ONNX Runtime + embedding models ───────────────
    items.push(check_onnx_embeddings(data_dir));

    // ── 4. Database ──────────────────────────────────────
    items.push(check_database(data_dir).await);

    // ── 5. LLM configuration ─────────────────────────────
    items.push(check_llm_config(data_dir));

    // ── 6. Permission / sandbox model ────────────────────
    items.push(check_permission_model());

    let total_ms = overall_start.elapsed().as_millis() as u64;
    let pass = items.iter().filter(|i| i.status == CheckStatus::Pass).count();
    let warn = items.iter().filter(|i| i.status == CheckStatus::Warn).count();
    let fail = items.iter().filter(|i| i.status == CheckStatus::Fail).count();

    let report = StartupReport { items, total_ms, pass, warn, fail };
    print_report(&report);
    report
}

// ── Individual Checks ────────────────────────────────────────────────────

fn check_data_dirs(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let required_dirs = [
        "db", "vectors", "graph", "sandbox",
        "memory/diary", "memory/facts", "memory/.dreams", "memory/wiki", "memory/vector",
        "models", "runtime", "domain", "domain/inbox",
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

    let detail = if missing.is_empty() {
        if created.is_empty() {
            format!("{} directories present", required_dirs.len())
        } else {
            format!("{} dirs OK, {} created", required_dirs.len() - created.len(), created.len())
        }
    } else {
        format!("Missing: {}", missing.join(", "))
    };

    let status = if missing.is_empty() { CheckStatus::Pass } else { CheckStatus::Fail };
    CheckItem { name: "Data directories", status, detail, latency_ms: start.elapsed().as_millis() as u64 }
}

async fn check_bootstrap(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let bs = everevo_bootstrap::Bootstrap::new(data_dir.to_path_buf());
    match bs.check().await {
        Ok(result) => {
            let runtime_ok = result.ready.iter().filter(|p| !p.key.starts_with("reranker") && !p.key.starts_with("bge") && !p.key.starts_with("all-")).count();
            let model_ok = result.ready.iter().filter(|p| p.key.starts_with("bge") || p.key.starts_with("all-") || p.key.starts_with("reranker")).count();

            let detail = format!(
                "{} runtimes + {} models ready, {} missing, {} corrupt",
                runtime_ok, model_ok,
                result.missing.len(), result.corrupt.len()
            );

            let status = if result.missing.is_empty() && result.corrupt.is_empty() {
                CheckStatus::Pass
            } else if result.ready.len() >= 4 {
                CheckStatus::Warn
            } else {
                CheckStatus::Fail
            };

            CheckItem { name: "Bootstrap assets", status, detail, latency_ms: start.elapsed().as_millis() as u64 }
        }
        Err(e) => CheckItem {
            name: "Bootstrap assets",
            status: CheckStatus::Fail,
            detail: format!("Check failed: {e}"),
            latency_ms: start.elapsed().as_millis() as u64,
        },
    }
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
        format!("ORT={}, loaded: [{}], failed: [{}]",
            if has_ort { "yes" } else { "not found" },
            loaded.join(", "), failed.join(", "))
    } else {
        format!("ORT={}, models: [{}], smoke: {}/{}",
            if has_ort { "yes" } else { "not found" },
            loaded.join(", "), smoke_ok, models.len())
    };

    let status = if smoke_ok == models.len() as u32 { CheckStatus::Pass }
        else if smoke_ok > 0 || has_ort { CheckStatus::Warn }
        else { CheckStatus::Warn };

    CheckItem { name: "ONNX Embeddings", status, detail, latency_ms: start.elapsed().as_millis() as u64 }
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
            // Close pool immediately — final connect happens later in startup
            db.pool.close().await;
            format!(
                "SQLite OK ({}), file {}KB",
                db_path.display(),
                size / 1024
            )
        }
        Err(e) => return CheckItem {
            name: "Database",
            status: CheckStatus::Fail,
            detail: format!("Connect failed: {e}"),
            latency_ms: start.elapsed().as_millis() as u64,
        },
    };

    CheckItem { name: "Database", status: CheckStatus::Pass, detail, latency_ms: start.elapsed().as_millis() as u64 }
}

fn check_llm_config(data_dir: &Path) -> CheckItem {
    let start = Instant::now();
    let config_path = data_dir.join("config.toml");

    if !config_path.exists() {
        return CheckItem {
            name: "LLM Configuration",
            status: CheckStatus::Warn,
            detail: "data/config.toml not found — bootstrap UI needed".into(),
            latency_ms: start.elapsed().as_millis() as u64,
        };
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            let has_llm = content.contains("[llm]") || content.contains("api_key");
            let has_anthropic = content.contains("ANTHROPIC_API_KEY") || content.contains("anthropic");
            let has_openai = content.contains("OPENAI_API_KEY") || content.contains("openai");

            let mut providers = Vec::new();
            if has_anthropic { providers.push("anthropic"); }
            if has_openai { providers.push("openai"); }

            let detail = if !providers.is_empty() {
                format!("config.toml found, providers: [{}]", providers.join(", "))
            } else if has_llm {
                "config.toml found, LLM section present".into()
            } else {
                "config.toml found, no LLM providers configured yet".into()
            };

            let status = if !providers.is_empty() { CheckStatus::Pass }
                else if has_llm { CheckStatus::Warn }
                else { CheckStatus::Warn };

            CheckItem { name: "LLM Configuration", status, detail, latency_ms: start.elapsed().as_millis() as u64 }
        }
        Err(e) => CheckItem {
            name: "LLM Configuration",
            status: CheckStatus::Warn,
            detail: format!("Cannot read config.toml: {e}"),
            latency_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn check_permission_model() -> CheckItem {
    let start = Instant::now();
    let rules = everevo_sandbox::PermissionRules::default();

    let detail = format!(
        "level={} ({}), {} dangerous + {} safe + {} admin + {} deny patterns, deny paths={}",
        rules.level.label(),
        if rules.scan_absolute_paths { "path-scan" } else { "no-path-scan" },
        rules.shell_dangerous_patterns.len(),
        rules.shell_safe_patterns.len(),
        rules.shell_admin_patterns.len(),
        rules.shell_deny_patterns.len(),
        rules.filesystem_write_denylist.len(),
    );

    CheckItem { name: "Permission Model", status: CheckStatus::Pass, detail, latency_ms: start.elapsed().as_millis() as u64 }
}

// ── Terminal Report ───────────────────────────────────────────────────────

fn print_report(report: &StartupReport) {
    // ANSI escapes for colored output
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

    let summary_color = if report.fail > 0 { red }
        else if report.warn > 0 { yellow }
        else { green };

    print!("  {summary_color}{} pass", report.pass);
    if report.warn > 0 { print!(", {} warn", report.warn); }
    if report.fail > 0 { print!(", {} fail", report.fail); }
    println!("{reset}  {dim}({}ms total){reset}", report.total_ms);

    if report.fail > 0 {
        println!("{red}  ⚡ Critical issues detected — system may not function correctly.{reset}");
    } else if report.warn > 0 {
        println!("{yellow}  ⚡ Warnings present — some features degraded.{reset}");
    } else {
        println!("{green}  ⚡ All systems nominal.{reset}");
    }
    println!();
}
