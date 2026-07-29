//! EverEvo server entry point — CLI parsing, bootstrap, serve.
//!
//! ```text
//! everevo serve       # Start the web server (default)
//! everevo bootstrap   # Check & provision runtimes + models
//! everevo chat <msg>  # Quick agent chat in terminal (Phase 2)
//! ```

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "everevo", version, about = "EverEvo AI Agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start backend + frontend dev server with full logging
    Dev {
        #[arg(long, env = "EVEREVO_HOST", default_value = "127.0.0.1")]
        host: String,
        #[arg(long, short, env = "EVEREVO_PORT", default_value = "3000")]
        port: u16,
        #[arg(long, default_value = "false")]
        no_frontend: bool,
        /// Launch in Tauri desktop window instead of browser
        #[arg(long, default_value = "false")]
        tauri: bool,
    },
    /// Start the web server (default)
    Serve {
        #[arg(long, env = "EVEREVO_HOST", default_value = "127.0.0.1")]
        host: String,
        #[arg(long, short, env = "EVEREVO_PORT", default_value = "3000")]
        port: u16,
    },
    /// Check and provision runtimes & embedding models
    Bootstrap {
        /// Only check, don't download
        #[arg(long)]
        check_only: bool,
    },
    /// Quick agent chat in terminal (Phase 2)
    Chat {
        /// The message to send
        message: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    // ── Observability ──────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("everevo=info")),
        )
        .init();

    // ── Config ─────────────────────────────────────────────────────
    let config = everevo_core::AppConfig::load().unwrap_or_else(|e| {
        tracing::error!("Failed to load config: {e}");
        std::process::exit(1);
    });

    // Point ORT to our vendored ONNX Runtime DLL so it doesn't load the
    // incompatible Windows ML version from C:\Windows\System32 (v1.17.1).
    // Must happen before any ONNX code; also done in run_startup_check for Tauri.
    everevo_vector::configure_ort_dylib(&config.data_dir);

    // ── CLI ────────────────────────────────────────────────────────
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Dev {
        host: config.server_host.clone(),
        port: config.server_port,
        no_frontend: false,
        tauri: false,
    }) {
        Command::Dev {
            host,
            port,
            no_frontend,
            tauri: use_tauri,
        } => {
            if use_tauri {
                tracing::info!("Launching Tauri desktop shell...");
                #[allow(clippy::disallowed_methods)]
                let status = std::process::Command::new("npx")
                    .args(["tauri", "dev"])
                    .status()
                    .unwrap_or_else(|_| {
                        eprintln!("Tauri CLI not found. Run: cd frontend && npm install -D @tauri-apps/cli@latest");
                        std::process::exit(1);
                    });
                std::process::exit(status.code().unwrap_or(1));
            }
            cmd_dev(config, &host, port, no_frontend).await;
        }
        Command::Serve { host, port } => {
            cmd_serve(config, &host, port).await;
        }
        Command::Bootstrap { check_only } => {
            cmd_bootstrap(&config, check_only).await;
        }
        Command::Chat { message } => {
            cmd_chat(&config, &message.join(" ")).await;
        }
    }
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_dev(config: everevo_core::AppConfig, host: &str, port: u16, no_frontend: bool) {
    tracing::info!("=== EverEvo Dev Mode ===");

    // Start frontend dev server as child process
    #[allow(unused_mut)]
    let mut frontend_child = if !no_frontend {
        let cwd = std::env::current_dir().unwrap_or_default();
        let frontend_dir = cwd.join("frontend");
        if frontend_dir.join("package.json").exists() {
            tracing::info!("Starting frontend dev server...");
            #[allow(clippy::disallowed_methods)]
            Some(
                tokio::process::Command::new("cmd")
                    .args(["/c", "npm", "run", "dev"])
                    .current_dir(&frontend_dir)
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("Failed to start frontend. Make sure Node.js is installed."),
            )
        } else {
            tracing::warn!("frontend/package.json not found, skipping frontend");
            None
        }
    } else {
        None
    };

    tracing::info!(%host, port, "Backend starting...");
    cmd_serve(config, host, port).await;

    if let Some(mut child) = frontend_child {
        let _ = child.kill().await;
    }
}

async fn cmd_serve(config: everevo_core::AppConfig, host: &str, port: u16) {
    tracing::info!(
        data_dir = %config.data_dir.display(),
        host, port,
        "EverEvo server starting"
    );

    // ── Database ───────────────────────────────────────────────
    let db_path = config.database_path();
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap_or_else(|e| {
            tracing::error!("Failed to create db dir {}: {e}", parent.display());
            std::process::exit(1);
        });
    }
    let db = everevo_db::Database::connect(&db_path)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Database connection failed at {}: {e}", db_path.display());
            std::process::exit(1);
        });
    tracing::info!(path = %db_path.display(), "Database connected");

    // ── Build app (creates InitPipeline + Downloader) ──────────
    let (app, state) = everevo_server::build_app(config.clone(), db)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Failed to build app: {e}");
            std::process::exit(1);
        });

    // ── Init orchestrator: provision assets → check LLM → startup checks ──
    let pipeline = state.init_pipeline.clone();
    let init_state = Arc::clone(&state);
    let data_dir_c = config.data_dir.clone();

    if !pipeline.is_initialized() {
        println!("[init] First boot — provisioning in background…");
        tracing::info!("First boot — provisioning in background…");
        let mut events = pipeline.events();
        let _handle = tokio::spawn(async move { pipeline.run().await });

        tokio::spawn(async move {
            // ── Phase 1: wait for asset provisioning ──
            while let Ok(event) = events.recv().await {
                match event {
                    everevo_bootstrap::pipeline::InitEvent::AssetDone { key, .. } => {
                        tracing::info!(%key, "Asset provisioned");
                    }
                    everevo_bootstrap::pipeline::InitEvent::AssetFailed { key, error, .. } => {
                        tracing::warn!(%key, %error, "Asset failed");
                    }
                    everevo_bootstrap::pipeline::InitEvent::AllDone => {
                        tracing::info!("All assets provisioned");
                        break;
                    }
                    everevo_bootstrap::pipeline::InitEvent::FatalError { error: e } => {
                        tracing::error!(%e, "Init pipeline fatal error");
                        break;
                    }
                    _ => {}
                }
            }
            println!("[init] Assets done, entering LLM phase…");

            // ── Phase 2: check LLM config (wait if needed) ──
            run_init_llm_phase(&init_state, &data_dir_c).await;
        });
    } else {
        println!("[init] Already provisioned — spawning LLM check task");
        tracing::info!("Init marker found — already provisioned");
        // Assets ready; run LLM check + startup checks in background.
        tokio::spawn(async move {
            run_init_llm_phase(&init_state, &data_dir_c).await;
        });
    }

    // ── Dreaming Scheduler ─────────────────────────────────────
    let scheduler_handle = state.scheduler.start_background(
        Arc::clone(&state.dreaming_engine),
        Arc::clone(&state.fact_manager),
        Arc::clone(&state.wiki_generator),
    );
    tracing::info!("Dreaming scheduler started");

    // ── Domain Global Inbox Watcher ───────────────────────────────
    // Monitors data/domain/inbox/ for new files every 2 minutes.
    // Documents are classified, chunked, and moved into named domains.
    let domain_root = state.config.data_dir.join("domain");
    let models_dir = state.config.data_dir.join("models");
    let _domain_watcher_handle = tokio::spawn(async move {
        // Load once with ONNX embedder (non-fatal if models unavailable).
        let mut mgr = match everevo_agent::knowledge::domain::DomainManager::load_with_onnx(
            &domain_root, &models_dir,
        ) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "DomainManager load_with_onnx failed, trying fallback");
                match everevo_agent::knowledge::domain::DomainManager::load(&domain_root) {
                    Ok(m) => m,
                    Err(e2) => {
                        tracing::error!(error = %e2, "Domain inbox watcher disabled — cannot load DomainManager");
                        return;
                    }
                }
            }
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(120));
        loop {
            interval.tick().await;
            match mgr.process_global_inbox().await {
                Ok(result) if result.processed > 0 => {
                    tracing::info!(
                        processed = result.processed,
                        new_domains = ?result.new_domains,
                        "Global inbox processed"
                    );
                }
                Ok(_) => {} // nothing new
                Err(e) => tracing::warn!(error = %e, "Global inbox processing failed"),
            }
        }
    });
    tracing::info!("Domain inbox watcher started");

    // ── RAG pipeline ────────────────────────────────────────────
    // HNSW vector store is pure Rust, no nested-runtime conflicts.
    // Facts auto-index on save (triple-write: MD + SQLite + Vector).
    if state.rag_pipeline.is_some() {
        tracing::info!("RAG pipeline active — vector search enabled (HNSW)");
    } else {
        tracing::info!("RAG pipeline unavailable — vector search disabled (non-fatal)");
    }

    let addr = format!("{host}:{port}");

    // ── Startup diagnostic summary ───────────────────────────────
    {
        let mcp_count = state.mcp_clients.read().await.len();
        let mcp: Vec<_> = state.mcp_clients.read().await.iter().map(|(n, c)| {
            let tools = c.try_lock().map(|g| g.tools.len()).unwrap_or(0);
            format!("{n}({tools}t)")
        }).collect();
        let llm = state.llm.read().await;
        let primary = llm.get("primary").and_then(|c| c.as_ref()).is_some();

        println!("╔══════════════════════════════════════════╗");
        println!("║        EverEvo Server v{}           ║", env!("CARGO_PKG_VERSION"));
        println!("╠══════════════════════════════════════════╣");
        println!("║ DB:    {}  ║", if std::path::Path::new(&format!("{}/everevo.db", config.data_dir.display())).exists() { "connected" } else { "new" });
        println!("║ LLM:   {}  ║", if primary { "primary configured" } else { "unconfigured" });
        println!("║ MCP:   {} server(s) {}  ║", mcp_count, if mcp.is_empty() { "(none)".to_string() } else { mcp.join(", ") });
        println!("║ Tools: {} built-in         ║", 17);
        println!("║ Addr:  http://{addr}     ║");
        println!("╚══════════════════════════════════════════╝");
    }

    tracing::info!(%addr, "Server listening → http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Failed to bind {addr}: {e}");
            std::process::exit(1);
        });

    // Auto-open browser
    let url = format!("http://{addr}");
    if std::path::Path::new("frontend/dist/index.html").exists() || cfg!(debug_assertions) {
        let _ = open::that(&url);
    }

    // Graceful shutdown on Ctrl+C / SIGTERM
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Shutdown signal received, draining connections...");
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Server error: {e}");
            std::process::exit(1);
        });

    // ── Cleanup ─────────────────────────────────────────────────
    // Drain in-flight connections before killing sandboxes.
    tracing::info!("Shutting down — draining connections (5s timeout)...");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    state.destroy_all_sandboxes().await;
    state.scheduler.stop();
    tracing::info!("Dreaming scheduler stopped, waiting for background task...");
    let _ = scheduler_handle.await;
    tracing::info!("Shutdown complete.");
}

async fn cmd_bootstrap(config: &everevo_core::AppConfig, check_only: bool) {
    println!("EverEvo Bootstrap");
    println!("  data dir: {}", config.data_dir.display());
    println!();

    let bootstrap = everevo_bootstrap::Bootstrap::new(config.data_dir.clone());

    match bootstrap.check().await {
        Ok(status) => {
            println!("Ready ({} assets):", status.ready.len());
            for r in &status.ready {
                println!("  ✅ {}  v{}  ({})", r.key, r.version, r.path.display());
            }

            if !status.corrupt.is_empty() {
                println!();
                println!(
                    "Corrupt ({} assets — re-download needed):",
                    status.corrupt.len()
                );
                for c in &status.corrupt {
                    println!("  ⚠️  {}  v{}", c.key, c.version);
                }
            }

            if !status.missing.is_empty() {
                println!();
                println!(
                    "Missing ({} assets, ~{} MB):",
                    status.missing.len(),
                    status.download_size_bytes / 1_048_576
                );
                for m in &status.missing {
                    println!("  ❌ {}  v{}  — {}", m.key, m.version, m.description);
                }

                if check_only {
                    println!();
                    println!("Run `everevo bootstrap` (without --check-only) to download.");
                } else {
                    println!();
                    println!("Downloading {} assets...", status.missing.len());

                    let dl_config = everevo_downloader::config::DownloaderConfig::default();
                    let dl = match everevo_downloader::Downloader::new(dl_config) {
                        Ok(d) => std::sync::Arc::new(d),
                        Err(e) => {
                            eprintln!("Failed to create downloader: {e}");
                            return;
                        }
                    };
                    let bs = std::sync::Arc::new(bootstrap);
                    let resource_dir = std::env::var("EVEREVO_RESOURCE_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_default();
                    let pipeline = everevo_bootstrap::pipeline::InitPipeline::new(
                        config.data_dir.clone(),
                        bs,
                        dl,
                        resource_dir,
                    );

                    let mut events = pipeline.events();
                    let handle = tokio::spawn(async move { pipeline.run().await });

                    while let Ok(event) = events.recv().await {
                        match event {
                            everevo_bootstrap::pipeline::InitEvent::DownloadProgress {
                                key,
                                percentage,
                                ..
                            } => {
                                println!("  ⬇ {key}: {percentage:.0}%");
                            }
                            everevo_bootstrap::pipeline::InitEvent::LayerStart {
                                key,
                                layer: 2,
                                ..
                            } => {
                                println!("  📦 {key}: extracting...");
                            }
                            everevo_bootstrap::pipeline::InitEvent::AssetDone {
                                key,
                                completed,
                                total,
                            } => {
                                println!("  ✅ {key} ({completed}/{total})");
                            }
                            everevo_bootstrap::pipeline::InitEvent::AssetFailed {
                                key,
                                error,
                                ..
                            } => {
                                eprintln!("  ❌ {key}: {error}");
                            }
                            everevo_bootstrap::pipeline::InitEvent::AllDone => {
                                println!("All assets provisioned.");
                                break;
                            }
                            everevo_bootstrap::pipeline::InitEvent::FatalError { error: e } => {
                                eprintln!("Fatal: {e}");
                                break;
                            }
                            _ => {}
                        }
                    }
                    let _ = handle.await;
                }
            } else if status.corrupt.is_empty() && status.ready.len() == 8 {
                println!();
                println!("All 8 assets ready. System is fully provisioned.");
            }
        }
        Err(e) => {
            eprintln!("Bootstrap check failed: {e}");
            std::process::exit(1);
        }
    }
}

async fn cmd_chat(config: &everevo_core::AppConfig, message: &str) {
    use everevo_agent::loop_::{AgentEvent, AgentLoop};
    use everevo_agent::tools::build_registry;
    use everevo_core::llm::LlmMessage;
    use everevo_sandbox::{SandboxConfig, TieredSandbox};
    use std::sync::Arc;

    // ── LLM Client ───────────────────────────────────────────────────
    let llm = match load_primary_llm(config).await {
        Some(client) => Arc::new(client),
        None => {
            eprintln!("No LLM configured. Add an [[llm]] entry to data/config.toml");
            std::process::exit(1);
        }
    };

    // ── Sandbox ──────────────────────────────────────────────────────
    let sandbox_root = config.data_dir.join("sandbox");
    std::fs::create_dir_all(&sandbox_root).ok();
    let sandbox = TieredSandbox::new(SandboxConfig {
        sandbox_root,
        ..Default::default()
    })
    .unwrap_or_else(|e| {
        eprintln!("Failed to create sandbox: {e}");
        std::process::exit(1);
    });
    let sandbox: Arc<dyn everevo_core::sandbox::SandboxProvider> = Arc::new(sandbox);

    // ── Tools ────────────────────────────────────────────────────────
    let tools = Arc::new(build_registry(
        sandbox, None, // no downloader for CLI chat
        None, // no bootstrap for CLI chat
    ));

    // ── Agent Loop ───────────────────────────────────────────────────
    let agent = AgentLoop::new().with_max_turns(30);
    let messages = vec![LlmMessage::user(message)];

    println!("🤖 EverEvo\n");

    let mut events = agent.run(llm, tools, messages, None).await;
    let mut final_text = String::new();

    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::TextDelta(delta) => {
                print!("{delta}");
                final_text.push_str(&delta);
            }
            AgentEvent::ToolCallStart {
                name, arguments, ..
            } => {
                println!("\n🔧 {name}({arguments})");
            }
            AgentEvent::ToolCallEnd {
                content, is_error, ..
            } => {
                if is_error {
                    println!("❌ {content}");
                }
            }
            AgentEvent::Done { .. } => {
                if !final_text.is_empty() {
                    println!();
                }
                break;
            }
            AgentEvent::Error { message } => {
                eprintln!("\n⚠️  {message}");
                break;
            }
            _ => {}
        }
    }
}

use everevo_server::main_impl::run_init_llm_phase;

/// Load the primary LLM provider from data/config.toml.
async fn load_primary_llm(
    config: &everevo_core::AppConfig,
) -> Option<everevo_agent::llm::HttpClient> {
    let path = config.data_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    let table: toml::Value = toml::from_str(&content).ok()?;
    let llm_arr = table.get("llm")?.as_array()?;
    let entry = llm_arr.first()?;

    let api_fmt = entry.get("api_format")?.as_str().unwrap_or("anthropic");
    let key = entry.get("api_key")?.as_str().unwrap_or("");
    let url = entry.get("base_url")?.as_str().unwrap_or("");
    let model = entry.get("model")?.as_str().unwrap_or("");

    if key.is_empty() {
        return None;
    }

    Some(everevo_agent::llm::HttpClient::new(
        api_fmt, key, url, model,
    ))
}
