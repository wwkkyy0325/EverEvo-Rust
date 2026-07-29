//! Shared application state injected into Axum handlers.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

use everevo_agent::knowledge::domain::DomainRegistry;
use everevo_agent::llm::HttpClient;
use everevo_agent::memory::diary::DiaryManager;
use everevo_agent::memory::facts::FactManager;
use everevo_agent::memory::scheduler::{DreamingScheduler, SchedulerConfig};
use everevo_agent::memory::wiki::WikiGenerator;
use everevo_agent::memory::DreamingEngine;
use everevo_agent::rag::RagPipeline;
use everevo_vector::{ModelRegistry, MultiCollectionStore};
use everevo_agent::skill::SkillRegistry;
use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};
use everevo_bootstrap::pipeline::InitPipeline;
use everevo_bootstrap::Bootstrap;
use everevo_core::context::ContextSnapshot;
use everevo_core::slash_command::SlashCommandRegistry;
use everevo_core::{AppConfig, EverEvoError};
use everevo_core::{Telemetry, TelemetryConfig};
use everevo_db::Database;
use everevo_downloader::Downloader;
use everevo_sandbox::{SandboxConfig, SessionSandbox};

/// Bundle of memory subsystem components returned by `init_memory`.
type MemoryStack = (
    Arc<FactManager>,
    Arc<DiaryManager>,
    Arc<DreamingScheduler>,
    Arc<DreamingEngine>,
    Arc<WikiGenerator>,
);

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
    pub knowledge_graph: Arc<std::sync::RwLock<everevo_agent::knowledge::KnowledgeGraph>>,
    /// Domain knowledge base registry.
    pub domain_registry: Arc<std::sync::RwLock<DomainRegistry>>,
    /// Telemetry — observability and metrics for agent sessions.
    pub telemetry: Arc<Telemetry>,
    /// Skill registry — scans data/skills/ for SKILL.md files.
    pub skill_registry: Arc<SkillRegistry>,
    /// Slash command registry — built-in + plugin slash commands for chat input.
    pub commands: Arc<SlashCommandRegistry>,
    /// Cached runtime environment — computed once at startup, reused for every
    /// sandbox creation to avoid repeated filesystem scans of .extracted sentinels.
    pub runtime_env: everevo_bootstrap::runtime::RuntimeEnv,
    /// Per-session cancellation tokens for interrupting active agent runs.
    pub session_actors: RwLock<HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>>,
    /// Sub-agent handles keyed by session UUID — supports listing and cancellation via API.
    pub subagent_handles: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentHandle>>>>>,
    /// Sub-agent status snapshots keyed by session UUID — for status API.
    pub subagent_statuses: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentStatus>>>>>,
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
        let knowledge_graph = Arc::new(std::sync::RwLock::new(
            everevo_agent::knowledge::KnowledgeGraph::open(&graph_dir)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to open knowledge graph, starting empty");
                    everevo_agent::knowledge::KnowledgeGraph::open(&graph_dir).unwrap()
                }),
        ));

        let (fact_manager, diary_manager, scheduler, dreaming_engine, wiki_generator) =
            Self::init_memory(&config, &llm, &knowledge_graph)?;

        // Wire SQLite FTS5 for fact keyword search (triple-write: MD + SQLite + Vector)
        fact_manager.set_db(Arc::new(db.clone()));

        // Model registry — auto-discovers ONNX models under data/models/.
        let model_registry = {
            let models_dir = config.data_dir.join("models");
            let preferred = config.embedding_model.as_deref();
            Arc::new(std::sync::RwLock::new(
                ModelRegistry::discover(models_dir, preferred).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Model registry init failed — RAG disabled");
                    // Return a dummy? No — ModelRegistry::discover fails if no models.
                    // This should never happen in production (models bundled).
                    panic!("No embedding models found — cannot start");
                })
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
                    Ok(n) => tracing::info!(count = n, "Backfilled existing facts into vector store"),
                    Err(e) => tracing::warn!(error = %e, "Fact vector backfill failed (non-fatal)"),
                }
            });
        }

        let telemetry = Self::init_telemetry(&config);
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
        everevo_agent::tools::builtins::load_persisted_tasks(
            &todo_store,
            &config.data_dir,
        )
        .await;

        let state = Arc::new(Self {
            config,
            db,
            llm: RwLock::new(llm),
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
            telemetry,
            skill_registry,
            commands,
            runtime_env,
            session_actors: RwLock::new(HashMap::new()),
            subagent_handles: RwLock::new(HashMap::new()),
            subagent_statuses: RwLock::new(HashMap::new()),
            mcp_clients: RwLock::new(HashMap::new()),
            bg_sessions: RwLock::new(HashMap::new()),
            workspace_dir: Arc::new(RwLock::new(workspace_dir)),
            plan_mode_sessions: Arc::new(RwLock::new(HashMap::new())),
            context_snapshots: RwLock::new(HashMap::new()),
            model_registry,
            rag_pipeline,
            project_vector_store,
        });
        // Connect to configured MCP servers (non-blocking, best-effort)
        Self::connect_mcp_servers(&state).await;
        // Start background MCP health checker
        Self::spawn_mcp_health_checker(&state);
        Ok(state)
    }

    /// Spawn a background task that periodically checks MCP server health
    /// and attempts reconnection for dead servers.
    ///
    /// Claude Code alignment: MCP servers that crash are automatically
    /// reconnected within 60 seconds without user intervention.
    fn spawn_mcp_health_checker(state: &Arc<Self>) {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            // Skip first tick (it fires immediately)
            interval.tick().await;
            loop {
                interval.tick().await;
                let dead: Vec<String> = {
                    let clients = state.mcp_clients.read().await;
                    clients
                        .iter()
                        .filter_map(|(name, client)| {
                            match client.try_lock() {
                                Ok(mut guard) => {
                                    if !guard.is_alive() {
                                        Some(name.clone())
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None, // busy — skip health check
                            }
                        })
                        .collect()
                };

                for name in &dead {
                    // Find config for this server
                    let srv = state
                        .config
                        .mcp_servers
                        .iter()
                        .find(|s| &s.name == name)
                        .cloned();

                    if let Some(srv) = srv {
                        if !srv.enabled {
                            continue;
                        }
                        // Drop dead client
                        state.mcp_clients.write().await.remove(name);
                        tracing::warn!(%name, "MCP server dead — attempting reconnect");

                        let result = match srv.transport.as_str() {
                            "http" | "sse" => {
                                everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await
                            }
                            _ => {
                                let args: Vec<&str> =
                                    srv.args.iter().map(String::as_str).collect();
                                everevo_mcp::discover_mcp_tools(&srv.command, &args, &srv.env).await
                            }
                        };

                        match result {
                            Ok((client, tools)) => {
                                tracing::info!(
                                    %name,
                                    tool_count = tools.len(),
                                    "MCP server auto-reconnected"
                                );
                                state
                                    .mcp_clients
                                    .write()
                                    .await
                                    .insert(name.clone(), client);
                            }
                            Err(e) => {
                                tracing::error!(
                                    %name,
                                    error = %e,
                                    "MCP auto-reconnect failed — will retry in 60s"
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    async fn connect_mcp_servers(state: &Arc<Self>) {
        for srv in &state.config.mcp_servers {
            if !srv.enabled {
                continue;
            }
            let result = match srv.transport.as_str() {
                "http" | "sse" => {
                    everevo_mcp::discover_mcp_tools_http(&srv.url, &srv.headers).await
                }
                _ => {
                    // stdio (default)
                    let args: Vec<&str> = srv.args.iter().map(String::as_str).collect();
                    everevo_mcp::discover_mcp_tools(&srv.command, &args, &srv.env).await
                }
            };

            match result {
                Ok((client, tools)) => {
                    tracing::info!(
                        name = %srv.name,
                        transport = %srv.transport,
                        tool_count = tools.len(),
                        "MCP server connected"
                    );
                    state
                        .mcp_clients
                        .write()
                        .await
                        .insert(srv.name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!(name = %srv.name, transport = %srv.transport, error = %e, "MCP server connection failed");
                }
            }
        }
    }

    fn init_downloader() -> Result<Arc<Downloader>, EverEvoError> {
        let default_region = match std::env::var("EVEREVO_DEFAULT_REGION")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "domestic" | "cn" => everevo_downloader::task::Region::Domestic,
            "international" | "intl" => everevo_downloader::task::Region::International,
            _ => everevo_downloader::task::Region::Auto,
        };
        let cfg = everevo_downloader::config::DownloaderConfig {
            max_concurrent_tasks: 4,
            timeout_secs: 0,
            mirror_enabled: true,
            default_region,
            ..Default::default()
        };
        Ok(Arc::new(Downloader::new(cfg).map_err(|e| {
            EverEvoError::Config(format!("Downloader: {e}"))
        })?))
    }

    fn init_memory(
        config: &AppConfig,
        llm: &HashMap<String, Option<Arc<HttpClient>>>,
        knowledge_graph: &Arc<std::sync::RwLock<everevo_agent::knowledge::KnowledgeGraph>>,
    ) -> Result<MemoryStack, EverEvoError> {
        let root = config.data_dir.join("memory");
        let fm = Arc::new(
            FactManager::new(root.join("facts"))
                .map_err(|e| EverEvoError::Config(format!("FactManager: {e}")))?,
        );
        let dm = Arc::new(
            DiaryManager::new(root.join("diary"))
                .map_err(|e| EverEvoError::Config(format!("DiaryManager: {e}")))?,
        );
        // Use ANY available LLM for the dreaming pipeline, not just "primary".
        // Falls back to first non-None entry if "primary" isn't configured.
        let primary = llm.get("primary")
            .and_then(|v| v.clone())
            .or_else(|| llm.values().find_map(|v| v.clone()));
        let sched = Arc::new(DreamingScheduler::new(SchedulerConfig::default()));
        let mut engine = DreamingEngine::new(
            Arc::clone(&dm),
            Arc::clone(&fm),
            primary.clone(),
            &root,
        )?;
        // Wire shared KG for DEEP phase entity extraction sync
        engine.set_knowledge_graph(Arc::clone(knowledge_graph));
        let engine = Arc::new(engine);
        let wiki = {
            let mut g = WikiGenerator::new(root.join("wiki"))
                .map_err(|e| EverEvoError::Config(format!("WikiGenerator: {e}")))?;
            if let Some(c) = primary {
                g = g.with_llm(c);
            }
            Arc::new(g)
        };
        Ok((fm, dm, sched, engine, wiki))
    }

    /// Initialize the RAG pipeline using the active model from registry.
    fn init_rag(config: &AppConfig, registry: &ModelRegistry) -> Option<Arc<RagPipeline>> {
        match RagPipeline::new(&config.data_dir, registry) {
            Ok(rag) => {
                tracing::info!(
                    model = %rag.model_name,
                    dim = rag.dim,
                    real_embeddings = rag.real_embeddings,
                    chunk_count = rag.total_count(),
                    "RAG pipeline initialized"
                );
                Some(Arc::new(rag))
            }
            Err(e) => {
                tracing::warn!(error = %e, "RAG pipeline init failed (non-fatal)");
                None
            }
        }
    }

    fn init_telemetry(config: &AppConfig) -> Arc<Telemetry> {
        Arc::new(
            Telemetry::new(TelemetryConfig {
                db_path: config.data_dir.join("telemetry").join("metrics.db"),
                ..Default::default()
            })
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Telemetry init failed — disabling");
                Telemetry::new(TelemetryConfig {
                    enabled: false,
                    ..Default::default()
                })
                .expect("disabled telemetry")
            }),
        )
    }

    fn init_domain(config: &AppConfig) -> Arc<std::sync::RwLock<DomainRegistry>> {
        let root = config.data_dir.join("domain");
        std::fs::create_dir_all(root.join("inbox")).ok();
        Arc::new(std::sync::RwLock::new(
            DomainRegistry::load(&root.join("domains.json")).unwrap_or(DomainRegistry {
                domains: HashMap::new(),
                embedding_dim: 384,
            }),
        ))
    }

    fn init_skills(config: &AppConfig) -> Arc<SkillRegistry> {
        let user_dir = config.data_dir.join("skills");
        std::fs::create_dir_all(&user_dir).ok();
        let registry = SkillRegistry::load(&user_dir).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "User skill registry load failed — using empty");
            SkillRegistry::empty()
        });
        // Always register built-in skills (embedded in binary). User skills
        // with the same name take precedence (they were loaded first; builtins
        // skip duplicates).
        Arc::new(registry.with_builtins())
    }

    fn init_commands() -> Arc<SlashCommandRegistry> {
        use everevo_core::slash_command::SlashCommand;
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "List all available commands"));
        reg.register(SlashCommand::new("clear", "Clear current session history"));
        reg.register(SlashCommand::new("compact", "Trigger context compaction").with_args("topic"));
        reg.register(SlashCommand::new("plan", "Enter plan mode for task planning; /plan cancel to exit").with_args("task"));
        reg.register(SlashCommand::new("memory", "Search persistent memory").with_args("query"));
        reg.register(SlashCommand::new("config", "Show current configuration status"));
        reg.register(SlashCommand::new("tasks", "Show current task list status"));
        reg.register(SlashCommand::new("doctor", "Run system diagnostics and show health report"));
        Arc::new(reg)
    }

    /// Create a sandbox for a session. Default level is SemiAuto.
    ///
    /// Uses the cached runtime_env (computed once at startup) to inject
    /// portable runtime paths. This avoids repeated filesystem scans on
    /// every session creation.
    pub async fn create_sandbox(
        &self,
        session_id: uuid::Uuid,
        level: everevo_sandbox::PermissionLevel,
    ) -> Result<(), EverEvoError> {
        let sandbox_root = self.config.data_dir.join("sandbox");
        let workspace = self.workspace_dir.read().await.clone();
        // Add workspace path to injected_paths so commands inside workspace
        // are auto-approved at SemiAuto (Claude Code alignment: inside workspace = free)
        let mut injected_paths = self.runtime_env.paths.clone();
        if let Some(ref ws) = workspace {
            if ws.is_dir() {
                injected_paths.push(ws.clone());
            }
        }
        let base_config = SandboxConfig {
            sandbox_root,
            injected_paths,
            injected_env: self.runtime_env.env_vars.clone().into_iter().collect::<Vec<_>>(),
            ..Default::default()
        };
        let mut sandbox = SessionSandbox::create(&session_id.to_string(), &base_config)?
            .with_workspace(workspace);
        sandbox.set_permission_level(level);
        self.sandboxes.write().await.insert(session_id, sandbox);
        Ok(())
    }

    /// Kill all active sandbox processes on server shutdown.
    /// Sessions can be resumed after restart — sandboxes are recreated lazily.
    pub async fn destroy_all_sandboxes(&self) {
        let mut sandboxes = self.sandboxes.write().await;
        let count = sandboxes.len();
        for (id, sandbox) in sandboxes.drain() {
            if let Err(e) = sandbox.destroy() {
                tracing::warn!(%id, error = %e, "Sandbox cleanup failed");
            }
        }
        tracing::info!(count, "All sandbox processes terminated");
    }

    /// Destroy a session's sandbox and audit trail. Called when a session is deleted.
    pub async fn destroy_sandbox(&self, session_id: uuid::Uuid) {
        if let Some(sandbox) = self.sandboxes.write().await.remove(&session_id) {
            if let Err(e) = sandbox.destroy() {
                tracing::warn!(%session_id, error = %e, "Failed to clean up sandbox");
            }
        }
    }

    /// Store a context snapshot for a session, evicting the oldest entry
    /// if the ring buffer is full (max 5 entries).
    pub async fn record_context_snapshot(&self, snapshot: ContextSnapshot) {
        const MAX_SNAPSHOTS: usize = 5;
        let mut map = self.context_snapshots.write().await;
        let entries = map.entry(snapshot.session_id).or_default();
        if entries.len() >= MAX_SNAPSHOTS {
            entries.remove(0); // evict oldest
        }
        entries.push(snapshot);
    }
}

/// Load persisted workspace from data/config/workspace.json (Claude Code alignment).
/// Returns None if the file doesn't exist or is malformed.
fn load_workspace_config(config: &AppConfig) -> Option<PathBuf> {
    let path = config.config_dir.join("workspace.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let p = PathBuf::from(json.get("workspace_dir")?.as_str()?);
    if p.is_dir() { Some(p) } else { None }
}

impl AppState {
    /// Read LLM provider configs from `data/config.toml` and build HttpClient instances.
    /// Returns empty map if the file doesn't exist or is malformed — the bootstrap UI
    /// will prompt the user to configure providers.
    async fn load_llm_from_file(config: &AppConfig) -> HashMap<String, Option<Arc<HttpClient>>> {
        let path = config.data_dir.join("config.toml");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let table: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return HashMap::new(),
        };
        let llm_arr = match table.get("llm").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return HashMap::new(),
        };

        let mut map = HashMap::new();
        for entry in llm_arr {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("primary");
            let api_fmt = entry
                .get("api_format")
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic");
            let key = entry.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
            let url = entry.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
            let model = entry.get("model").and_then(|v| v.as_str()).unwrap_or("");

            if !key.is_empty() {
                let client = HttpClient::new(api_fmt, key, url, model);
                map.insert(id.to_string(), Some(Arc::new(client)));
            }
        }
        map
    }
}
