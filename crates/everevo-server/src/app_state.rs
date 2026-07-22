//! Shared application state injected into Axum handlers.

use std::collections::HashMap;
use std::sync::Arc;
use serde::Serialize;
use tokio::sync::{Notify, RwLock};

use everevo_agent::llm::HttpClient;
use everevo_agent::memory::diary::DiaryManager;
use everevo_agent::memory::facts::FactManager;
use everevo_agent::skill::SkillRegistry;
use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};
use everevo_domain::DomainRegistry;
use everevo_agent::memory::scheduler::{DreamingScheduler, SchedulerConfig};
use everevo_agent::memory::DreamingEngine;
use everevo_agent::memory::wiki::WikiGenerator;
use everevo_bootstrap::Bootstrap;
use everevo_bootstrap::pipeline::InitPipeline;
use everevo_core::{AppConfig, EverEvoError};
use everevo_db::Database;
use everevo_downloader::Downloader;
use everevo_sandbox::{SandboxConfig, SessionSandbox};
use everevo_telemetry::{Telemetry, TelemetryConfig};

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
    /// Domain knowledge base registry.
    pub domain_registry: Arc<std::sync::RwLock<DomainRegistry>>,
    /// Telemetry — observability and metrics for agent sessions.
    pub telemetry: Arc<Telemetry>,
    /// Skill registry — scans data/skills/ for SKILL.md files.
    pub skill_registry: Arc<SkillRegistry>,
    /// Per-session cancellation tokens for interrupting active agent runs.
    pub session_actors: RwLock<HashMap<uuid::Uuid, tokio_util::sync::CancellationToken>>,
    /// Sub-agent handles keyed by session UUID — supports listing and cancellation via API.
    pub subagent_handles: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentHandle>>>>>,
    /// Sub-agent status snapshots keyed by session UUID — for status API.
    pub subagent_statuses: RwLock<HashMap<uuid::Uuid, Arc<std::sync::Mutex<Vec<SubAgentStatus>>>>>,
}

impl AppState {
    pub async fn new(config: AppConfig, db: Database) -> Result<Arc<Self>, EverEvoError> {
        let bootstrap = Arc::new(Bootstrap::new(config.data_dir.clone()));
        let dl_config = everevo_downloader::config::DownloaderConfig {
            max_concurrent_tasks: 4,
            timeout_secs: 0,         // no global limit — per-task timeout handles it
            mirror_enabled: true,  // enable CN mirrors for GitHub/timeout fallback
            ..Default::default()
        };
        let downloader = Arc::new(Downloader::new(dl_config)
            .map_err(|e| EverEvoError::Config(format!("Downloader: {e}")))?);

        let init_pipeline = Arc::new(InitPipeline::new(
            config.data_dir.clone(),
            Arc::clone(&bootstrap),
            Arc::clone(&downloader),
        ));

        // Load persisted LLM providers from data/config.toml
        let llm = Self::load_llm_from_file(&config).await;

        // Ensure sandbox root exists (per-session dirs created lazily)
        let sandbox_root = config.data_dir.join("sandbox");
        std::fs::create_dir_all(&sandbox_root).ok();

        // Initialize memory subsystems
        let memory_root = config.data_dir.join("memory");
        let fact_manager = Arc::new(
            FactManager::new(memory_root.join("facts"))
                .map_err(|e| EverEvoError::Config(format!("FactManager: {e}")))?
        );
        let diary_manager = Arc::new(
            DiaryManager::new(memory_root.join("diary"))
                .map_err(|e| EverEvoError::Config(format!("DiaryManager: {e}")))?
        );
        let primary_llm = llm.get("primary").and_then(|v| v.clone());
        let scheduler = Arc::new(DreamingScheduler::new(SchedulerConfig::default()));
        let dreaming_engine = Arc::new(DreamingEngine::new(
            Arc::clone(&diary_manager),
            Arc::clone(&fact_manager),
            primary_llm.clone(),
            &memory_root,
        )?);
        let wiki_generator = {
            let mut gen = WikiGenerator::new(memory_root.join("wiki"))
                .map_err(|e| EverEvoError::Config(format!("WikiGenerator: {e}")))?;
            if let Some(client) = primary_llm {
                gen = gen.with_llm(client);
            }
            Arc::new(gen)
        };

        // ── Telemetry ──────────────────────────────────────────
        let telemetry = Arc::new(
            Telemetry::new(TelemetryConfig {
                db_path: config.data_dir.join("telemetry").join("metrics.db"),
                ..Default::default()
            })
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to initialize telemetry, disabling");
                Telemetry::new(TelemetryConfig {
                    enabled: false,
                    ..Default::default()
                })
                .expect("disabled telemetry should always construct")
            }),
        );

        // ── Domain Knowledge Base ────────────────────────────────
        let domain_root = config.data_dir.join("domain");
        std::fs::create_dir_all(domain_root.join("inbox")).ok();
        let domain_registry_path = domain_root.join("domains.json");
        let domain_registry = Arc::new(std::sync::RwLock::new(
            DomainRegistry::load(&domain_registry_path).unwrap_or(DomainRegistry {
                domains: HashMap::new(),
                embedding_dim: 384,
            })
        ));

        // ── Skill Registry ───────────────────────────────────────
        let skills_dir = config.data_dir.join("skills");
        std::fs::create_dir_all(&skills_dir).ok();
        let skill_registry = Arc::new(
            SkillRegistry::load(&skills_dir)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to load skill registry");
                    SkillRegistry::load(&std::path::PathBuf::new()).unwrap_or_else(|e| {
                        tracing::error!(error = %e, "SkillRegistry fallback load failed — skills disabled");
                        // Create a temp dir for the fallback — load() succeeds on empty dirs
                        let tmp = std::env::temp_dir().join("everevo_skills_fallback");
                        let _ = std::fs::create_dir_all(&tmp);
                        SkillRegistry::load(&tmp).unwrap_or_else(|e2| {
                            tracing::error!(error = %e2, "SkillRegistry unrecoverable");
                            panic!("SkillRegistry: cannot recover from load failure: {e2}");
                        })
                    })
                })
        );

        Ok(Arc::new(Self {
            config, db,
            llm: RwLock::new(llm),
            bootstrap, downloader, init_pipeline,
            init_phase: RwLock::new(InitPhase::Provisioning),
            llm_notify: Notify::new(),
            sandboxes: RwLock::new(HashMap::new()),
            confirmations: Arc::new(RwLock::new(HashMap::new())),
            fact_manager,
            diary_manager,
            scheduler,
            dreaming_engine,
            wiki_generator,
            domain_registry,
            telemetry,
            skill_registry,
            session_actors: RwLock::new(HashMap::new()),
            subagent_handles: RwLock::new(HashMap::new()),
            subagent_statuses: RwLock::new(HashMap::new()),
        }))
    }

    /// Create a sandbox for a session. Default level is SemiAuto.
    pub async fn create_sandbox(
        &self,
        session_id: uuid::Uuid,
        level: everevo_sandbox::PermissionLevel,
    ) -> Result<(), EverEvoError> {
        let sandbox_root = self.config.data_dir.join("sandbox");
        let base_config = SandboxConfig {
            sandbox_root,
            ..Default::default()
        };
        let mut sandbox = SessionSandbox::create(&session_id.to_string(), &base_config)?;
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
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("primary");
            let api_fmt = entry.get("api_format").and_then(|v| v.as_str()).unwrap_or("anthropic");
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
