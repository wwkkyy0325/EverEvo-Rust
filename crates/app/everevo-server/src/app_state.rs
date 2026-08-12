//! Shared application state injected into Axum handlers.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

use everevo_agent::llm::HttpClient;
use everevo_agent::memory::diary::DiaryManager;
use everevo_agent::memory::facts::FactManager;
use everevo_agent::memory::scheduler::DreamingScheduler;
use everevo_agent::memory::wiki::WikiGenerator;
use everevo_agent::memory::DreamingEngine;
use everevo_agent::rag::RagPipeline;
use everevo_agent::skill::SkillRegistry;
use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};
use everevo_bootstrap::pipeline::InitPipeline;
use everevo_bootstrap::Bootstrap;
use everevo_core::context::ContextSnapshot;
use everevo_knowledge::domain::DomainRegistry;
use everevo_vector::{ModelRegistry, MultiCollectionStore};

use everevo_core::slash_command::SlashCommandRegistry;
use everevo_core::{AppConfig, EverEvoError, TelemetryPipeline};
use everevo_db::Database;
use everevo_downloader::Downloader;
use everevo_sandbox::SessionSandbox;

mod init;
mod mcp;
mod providers;
mod sandbox;
mod session;
pub use providers::ResolvedProvider;

// ── Init Phase ──────────────────────────────────────────────────────────

/// Tracks where the server is in the startup initialization sequence.
/// The frontend polls `/api/init/status` to decide which splash screen to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitPhase {
    /// Downloading runtimes + embedding models.
    Provisioning,
    /// Assets ready, but no LLM provider configured — frontend shows config prompt.
    WaitingForLlm,
    /// LLM confirmed, running startup self-checks (ONNX, DB, permissions).
    Checking,
    /// All subsystems ready; frontend transitions to main app.
    Ready,
}

/// A pending confirmation that blocks a shell tool until the user responds.
pub struct PendingConfirmation {
    pub command: String,
    pub reason: String,
    /// Send `true` to approve, `false` to deny.
    pub response_tx: tokio::sync::oneshot::Sender<bool>,
}

/// Notification sent to the SSE stream when a tool needs user confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmationNotification {
    pub session_id: uuid::Uuid,
    pub command: String,
    pub reason: String,
}

pub struct AppState {
    pub config: AppConfig,
    pub db: Database,
    /// Multi-provider LLM clients, keyed by id ("primary", "secondary", ...).
    pub llm: RwLock<HashMap<String, Option<Arc<HttpClient>>>>,
    /// Vision provider — serves the `describe_image` tool. None → tool falls
    /// back to deterministic offline scripts (chess_fen.py / fractions_ocr.py).
    pub vision_llm: RwLock<Option<ResolvedProvider>>,
    /// Compaction provider — used for rolling-summary / autocompact. None →
    /// the main execution model is reused ("有哪个用哪个").
    pub compact_llm: RwLock<Option<ResolvedProvider>>,
    /// Meta-agent self-diagnosis toggle — routing config `metaAgentEnabled`
    /// (product default ON). `EVEREVO_META_AGENT` env wins; benchmark mode
    /// defaults OFF. See [`meta_agent_effective`].
    pub meta_agent_enabled: RwLock<bool>,
    pub bootstrap: Arc<Bootstrap>,
    pub downloader: Arc<Downloader>,
    /// Shared todo store — TodoWrite tool reads/writes per-session task lists.
    pub todo_store: everevo_agent::tools::builtins::TodoStore,
    /// Init pipeline — event-driven bootstrap orchestration.
    pub init_pipeline: Arc<InitPipeline>,
    /// Current startup phase (polled by frontend via GET /api/init/status).
    pub init_phase: RwLock<InitPhase>,
    /// Woken when the user configures an LLM provider during WaitingForLlm.
    pub llm_notify: Notify,
    /// Per-session sandboxes, keyed by session UUID.
    pub sandboxes: RwLock<HashMap<uuid::Uuid, SessionSandbox>>,
    /// Pending confirmations awaiting user response, keyed by session UUID.
    /// Shared between the chat route (tool blocks here) and the confirm endpoint.
    pub confirmations: Arc<RwLock<HashMap<uuid::Uuid, PendingConfirmation>>>,
    /// Fact manager — facts/ directory, MEMORY.md index.
    pub fact_manager: Arc<FactManager>,
    /// Diary manager — diary/ directory, LIGHT phase output.
    pub diary_manager: Arc<DiaryManager>,
    /// Dreaming scheduler — background consolidation pipeline trigger.
    pub scheduler: Arc<DreamingScheduler>,
    /// Dreaming engine — executes consolidation phases (wraps diary + fact managers).
    pub dreaming_engine: Arc<DreamingEngine>,
    /// Wiki generator — auto-creates wiki/*.md from memory facts.
    pub wiki_generator: Arc<WikiGenerator>,
    /// Knowledge graph — entities + relations via Oxigraph SPARQL.
    /// Shared across requests; loaded from data/memory/graph/knowledge.ttl.
    pub knowledge_graph: Arc<std::sync::RwLock<everevo_knowledge::KnowledgeGraph>>,
    /// Domain knowledge base registry.
    pub domain_registry: Arc<std::sync::RwLock<DomainRegistry>>,
    /// Telemetry — registered emission pipeline + sink for agent sessions.
    pub telemetry_pipeline: Arc<TelemetryPipeline>,
    /// Skill registry — scans data/skills/ for SKILL.md files.
    pub skill_registry: Arc<SkillRegistry>,
    /// Slash command registry — built-in + plugin slash commands for chat input.
    pub commands: Arc<SlashCommandRegistry>,
    /// Credential config removed — sandbox inherits host git config directly.
    /// Cached runtime environment — computed once at startup, reused for every
    /// sandbox creation to avoid repeated filesystem scans of .extracted sentinels.
    pub runtime_env: everevo_bootstrap::runtime::RuntimeEnv,
    /// Per-session cancellation tokens for interrupting active agent runs.
    pub session_actors: RwLock<HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>>,
    /// Sub-agent handles keyed by session UUID — supports listing and cancellation via API.
    pub subagent_handles: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentHandle>>>>>,
    /// Sub-agent status snapshots keyed by session UUID — for status API.
    pub subagent_statuses: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentStatus>>>>>,
    /// Plugin registry — version management, canary routing, self-repair tools.
    pub plugin_registry: Arc<everevo_kernel::PluginRegistry>,
    /// MCP (Model Context Protocol) clients, keyed by server name. Each client wraps
    /// a connected MCP server process and exposes its tools as everevo Tool adapters.
    pub mcp_clients: RwLock<HashMap<String, Arc<tokio::sync::Mutex<everevo_mcp::McpClient>>>>,
    /// Background session worker pool — tracks daemon sessions by session UUID.
    /// When a background session is running, its JoinHandle is stored here.
    /// On completion, the handle is removed and status updated to Completed in DB.
    pub bg_sessions: RwLock<HashMap<uuid::Uuid, tokio::task::JoinHandle<()>>>,
    /// Global default workspace directory. When set, all new sessions use this
    /// as their primary working directory. Overridable per-session via API.
    pub workspace_dir: Arc<RwLock<Option<PathBuf>>>,
    /// A2A (Agent-to-Agent) gateway — JSON-RPC 2.0 endpoint for external agents.
    pub a2a_gateway: Arc<everevo_a2a::A2aGateway>,
    /// Per-session plan mode state. None = normal mode.
    /// When a session is in plan mode, the value stores the pre-plan
    /// permission level for restoration on exit.
    /// Arc-wrapped so it can be shared with plan mode tools.
    pub plan_mode_sessions: Arc<RwLock<HashMap<uuid::Uuid, String>>>,
    /// Context injection observability — per-session ring buffers of recent
    /// context snapshots (max 5 entries per session).
    pub context_snapshots: RwLock<HashMap<uuid::Uuid, Vec<ContextSnapshot>>>,
    /// Cached startup check report — run once after init, served via health API.
    pub startup_report: Arc<tokio::sync::RwLock<Option<crate::startup_check::StartupReport>>>,
    /// Model registry — auto-discovers ONNX embedding models.
    pub model_registry: Arc<std::sync::RwLock<ModelRegistry>>,
    /// RAG pipeline — ONNX embeddings + HNSW vector store for semantic search.
    /// None if ONNX models are unavailable (falls back to keyword-only search).
    pub rag_pipeline: Option<Arc<RagPipeline>>,
    /// Per-project vector store at `{workspace}/.everevo/vector/`.
    /// Holds `code` and `domain` namespaces — isolated from global `data/vector/`
    /// (memory + wiki). Created lazily; None if no workspace is set or if the
    /// .everevo directory doesn't exist.
    pub project_vector_store: Option<Arc<MultiCollectionStore>>,
}

impl AppState {
    pub async fn new(config: AppConfig, db: Database) -> Result<Arc<Self>, EverEvoError> {
        let bootstrap = Arc::new(Bootstrap::new(config.data_dir.clone()));
        let downloader = Self::init_downloader()?;
        let resource_dir = std::env::var("EVEREVO_RESOURCE_DIR")
            .map(PathBuf::from)
            .unwrap_or_default();
        let init_pipeline = Arc::new(InitPipeline::new(
            config.data_dir.clone(),
            Arc::clone(&bootstrap),
            Arc::clone(&downloader),
            resource_dir,
        ));
        let llm = Self::load_llm_from_file(&config).await;
        std::fs::create_dir_all(config.data_dir.join("sandbox")).ok();

        // Ensure graph/ directory exists and open knowledge graph (before init_memory
        // so it can be passed to the DreamingEngine for DEEP phase KG sync)
        let graph_dir = config.data_dir.join("memory").join("graph");
        std::fs::create_dir_all(&graph_dir).ok();
        let knowledge_graph = Arc::new(std::sync::RwLock::new({
            let mut kg = everevo_knowledge::KnowledgeGraph::open(&graph_dir).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to open knowledge graph, starting empty");
                everevo_knowledge::KnowledgeGraph::open(&graph_dir).unwrap()
            });
            // Seed project structure on first open so `memory kg_search` has
            // entities to return from the very first query.
            kg.seed_project_structure(&[
                "everevo-core",
                "everevo-agent",
                "everevo-server",
                "everevo-db",
                "everevo-sandbox",
                "everevo-vector",
                "everevo-knowledge",
                "everevo-bootstrap",
                "everevo-downloader",
                "everevo-mcp",
                "everevo-workflow",
                "everevo-bundler",
            ]);
            kg
        }));

        let (fact_manager, diary_manager, scheduler, dreaming_engine, wiki_generator) =
            Self::init_memory(&config, &llm, &knowledge_graph)?;

        // Start the background dreaming scheduler — was never started before!
        // Without this, LIGHT (diary), REM (themes), DEEP (consolidation),
        // wiki generation, and persona updates NEVER run. All memory features
        // are read-only without the scheduler ticking.
        let persona_profile = config
            .data_dir
            .join("memory")
            .join("persona")
            .join("profile.json");
        // Benchmark mode (EVEREVO_BENCHMARK=1) skips the dreaming scheduler —
        // its DEEP phase promotes ALL sessions' content into shared global
        // facts + KG, which would leak answers across GAIA questions.
        if std::env::var("EVEREVO_BENCHMARK").is_err() {
            scheduler.start_background(
                Arc::clone(&dreaming_engine),
                Arc::clone(&fact_manager),
                Arc::clone(&wiki_generator),
                Some(persona_profile),
            );
            tracing::info!("Dreaming scheduler started (LIGHT/REM/DEEP + wiki + persona)");
        }

        // Wire SQLite FTS5 for fact keyword search (triple-write: MD + SQLite + Vector)
        fact_manager.set_db(Arc::new(db.clone()));
        // Serialized FTS5 writer actor — all fact upserts flow through a single
        // consumer, eliminating concurrent external-content trigger conflicts
        // (root cause of "SQL logic error" under burst saves). See:
        // https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/
        let fact_tx = Self::spawn_fact_writer(db.clone());
        fact_manager.set_write_queue(fact_tx);

        // Model registry — auto-discovers ONNX models under data/models/.
        let model_registry = {
            let models_dir = config.data_dir.join("models");
            let preferred = config.embedding_model.as_deref();
            Arc::new(std::sync::RwLock::new(
                ModelRegistry::discover(models_dir, preferred).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "No embedding models found — RAG disabled, starting without models");
                    ModelRegistry::empty()
                }),
            ))
        };

        // Wire RAG pipeline for vector search (triple-write: MD + SQLite + Vector).
        let rag_pipeline = {
            let reg = model_registry.read().unwrap_or_else(|e| e.into_inner());
            Self::init_rag(&config, &reg)
        };
        if let Some(ref rag) = rag_pipeline {
            fact_manager.set_rag(Arc::clone(rag));
            wiki_generator.set_rag(Arc::clone(rag));
            // Backfill: index existing facts (created before RAG was wired) into vectors.
            let fm = Arc::clone(&fact_manager);
            let r = Arc::clone(rag);
            tokio::spawn(async move {
                match fm.index_into_rag(&r) {
                    Ok(n) => {
                        tracing::info!(count = n, "Backfilled existing facts into vector store")
                    }
                    Err(e) => tracing::warn!(error = %e, "Fact vector backfill failed (non-fatal)"),
                }
            });
        }

        let telemetry_pipeline = Self::init_telemetry(&config);
        let domain_registry = Self::init_domain(&config);
        let skill_registry = Self::init_skills(&config);
        let commands = Self::init_commands();
        // Pre-compute runtime env once — avoids repeated filesystem scans
        // on every session creation (hot path).
        let runtime_env = bootstrap.build_runtime_env().await;

        // Workspace resolution: persisted file > config.toml > auto-detect CWD
        let workspace_dir = load_workspace_config(&config)
            .or_else(|| config.workspace_dir.clone())
            .or_else(|| std::env::current_dir().ok());

        // Per-project vector store at {workspace}/.everevo/vector/
        let project_vector_store = workspace_dir.as_ref().and_then(|ws| {
            let everevo_dir = ws.join(".everevo");
            if everevo_dir.exists() {
                match MultiCollectionStore::open(everevo_dir.join("vector"), 384, None) {
                    Ok(store) => {
                        tracing::info!(path = %everevo_dir.display(), "Project vector store opened");
                        Some(Arc::new(store))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Project vector store init failed (non-fatal)");
                        None
                    }
                }
            } else {
                None
            }
        });

        // Todo store with disk persistence — survives server restarts
        let todo_store = everevo_agent::tools::builtins::new_todo_store();
        everevo_agent::tools::builtins::load_persisted_tasks(&todo_store, &config.data_dir).await;

        // ── A2A Gateway ─────────────────────────────────────────────
        let a2a_gateway = {
            let base_url = format!("http://{}:{}", config.server_host, config.server_port);
            let a2a_config = everevo_a2a::A2aGatewayConfig {
                base_url,
                max_turns: 50,
                enable_auth: false, // dev mode — no auth
                ..Default::default()
            };
            // Use primary LLM if available; A2A tasks get chat capability
            let primary = llm
                .get("primary")
                .and_then(|v| v.clone())
                .or_else(|| llm.values().find_map(|v| v.clone()));
            if let Some(llm_client) = primary {
                let tools = Arc::new(everevo_core::tool::ToolRegistry::new());
                Arc::new(everevo_a2a::A2aGateway::new(llm_client, tools, a2a_config))
            } else {
                tracing::warn!("A2A gateway: no LLM available — gateway created without executor");
                let _tools = Arc::new(everevo_core::tool::ToolRegistry::new());
                let executor = Arc::new(everevo_a2a::executor::EchoExecutor);
                Arc::new(everevo_a2a::A2aGateway::with_executor(executor, a2a_config))
            }
        };

        // ── Initialize kernel plugin registry ──
        let plugin_registry =
            match everevo_kernel::PluginRegistry::open(config.data_dir.join("plugins")).await {
                Ok(reg) => Arc::new(reg),
                Err(e) => {
                    tracing::warn!(error = %e, "Plugin registry unavailable — using fallback");
                    Arc::new(
                        everevo_kernel::PluginRegistry::open(
                            std::env::temp_dir().join("everevo-plugins"),
                        )
                        .await
                        .expect("fallback plugin registry"),
                    )
                }
            };

        // Meta-agent toggle from routing config (default ON; benchmark/env handled
        // at request time in `meta_agent_effective`).
        let meta_agent_enabled = std::fs::read_to_string(config.data_dir.join("config.toml"))
            .ok()
            .and_then(|s| toml::from_str::<crate::routes::config::AppSettings>(&s).ok())
            .and_then(|s| s.routing)
            .map(|r| r.meta_agent_enabled)
            .unwrap_or(true);

        let state = Arc::new(Self {
            config,
            db,
            llm: RwLock::new(llm),
            vision_llm: RwLock::new(None),
            compact_llm: RwLock::new(None),
            meta_agent_enabled: RwLock::new(meta_agent_enabled),
            bootstrap,
            downloader,
            init_pipeline,
            todo_store,
            startup_report: Arc::new(tokio::sync::RwLock::new(None)),
            init_phase: RwLock::new(InitPhase::Provisioning),
            llm_notify: Notify::new(),
            sandboxes: RwLock::new(HashMap::new()),
            confirmations: Arc::new(RwLock::new(HashMap::new())),
            fact_manager,
            diary_manager,
            scheduler,
            dreaming_engine,
            wiki_generator,
            knowledge_graph,
            domain_registry,
            telemetry_pipeline,
            skill_registry,
            commands,
            runtime_env,
            session_actors: RwLock::new(HashMap::new()),
            subagent_handles: RwLock::new(HashMap::new()),
            subagent_statuses: RwLock::new(HashMap::new()),
            plugin_registry,
            mcp_clients: RwLock::new(HashMap::new()),
            bg_sessions: RwLock::new(HashMap::new()),
            workspace_dir: Arc::new(RwLock::new(workspace_dir)),
            a2a_gateway,
            plan_mode_sessions: Arc::new(RwLock::new(HashMap::new())),
            context_snapshots: RwLock::new(HashMap::new()),
            model_registry,
            rag_pipeline,
            project_vector_store,
        });
        // Connect to configured MCP servers (non-blocking, best-effort)
        Self::connect_mcp_servers(&state).await;
        // Start built-in webagent (best-effort, non-blocking)
        Self::start_webagent(&state).await;
        // Start background MCP health checker
        Self::spawn_mcp_health_checker(&state);
        // Resolve vision/compact special providers from routing config
        state.resolve_special_providers().await;
        Ok(state)
    }
}

/// Effective meta-agent switch for a request. Precedence:
/// 1. `EVEREVO_META_AGENT=0/1` env — explicit override, wins always.
/// 2. Benchmark mode (EVEREVO_BENCHMARK=1) — defaults OFF (unproven overhead,
///    extra tokens, and injected `[META-AGENT HINT]` interference under
///    convergence pressure).
/// 3. Routing config `metaAgentEnabled` — product default ON.
pub async fn meta_agent_effective(state: &AppState) -> bool {
    if let Ok(v) = std::env::var("EVEREVO_META_AGENT") {
        return v != "0";
    }
    if std::env::var("EVEREVO_BENCHMARK").is_ok() {
        return false;
    }
    *state.meta_agent_enabled.read().await
}

/// Load persisted workspace from data/config/workspace.json (Claude Code alignment).
/// Returns None if the file doesn't exist or is malformed.
fn load_workspace_config(config: &AppConfig) -> Option<PathBuf> {
    let path = config.config_dir.join("workspace.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let p = PathBuf::from(json.get("workspace_dir")?.as_str()?);
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}
