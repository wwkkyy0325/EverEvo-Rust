use std::{collections::HashMap, sync::Arc};

use everevo_agent::llm::HttpClient;
use everevo_agent::memory::diary::DiaryManager;
use everevo_agent::memory::facts::{FactManager, FactWriteTx};
use everevo_agent::memory::scheduler::{DreamingScheduler, SchedulerConfig};
use everevo_agent::memory::wiki::WikiGenerator;
use everevo_agent::memory::DreamingEngine;
use everevo_agent::rag::RagPipeline;
use everevo_agent::skill::SkillRegistry;
use everevo_core::slash_command::SlashCommandRegistry;
use everevo_core::{
    default_telemetry_pipeline, AppConfig, EverEvoError, Telemetry, TelemetryConfig,
    TelemetryPipeline,
};
use everevo_db::Database;
use everevo_downloader::Downloader;
use everevo_knowledge::domain::DomainRegistry;
use everevo_vector::ModelRegistry;

use super::AppState;

/// Bundle of memory subsystem components returned by `init_memory`.
pub(crate) type MemoryStack = (
    Arc<FactManager>,
    Arc<DiaryManager>,
    Arc<DreamingScheduler>,
    Arc<DreamingEngine>,
    Arc<WikiGenerator>,
);

impl AppState {
    pub(crate) fn init_downloader() -> Result<Arc<Downloader>, EverEvoError> {
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

    pub(crate) fn init_memory(
        config: &AppConfig,
        llm: &HashMap<String, Option<Arc<HttpClient>>>,
        knowledge_graph: &Arc<std::sync::RwLock<everevo_knowledge::KnowledgeGraph>>,
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
        let primary = llm
            .get("primary")
            .and_then(|v| v.clone())
            .or_else(|| llm.values().find_map(|v| v.clone()));
        let sched = Arc::new(DreamingScheduler::new(SchedulerConfig::default()));
        let mut engine =
            DreamingEngine::new(Arc::clone(&dm), Arc::clone(&fm), primary.clone(), &root)?;
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
    pub(crate) fn init_rag(
        config: &AppConfig,
        registry: &ModelRegistry,
    ) -> Option<Arc<RagPipeline>> {
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

    pub(crate) fn init_telemetry(config: &AppConfig) -> Arc<TelemetryPipeline> {
        // Wrap the sink in the default registered emission pipeline so record
        // producers are injected through `with_telemetry` instead of scattered
        // `record_*()` call sites.
        let sink = Telemetry::new(TelemetryConfig {
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
        });
        Arc::new(default_telemetry_pipeline(Arc::new(sink)))
    }

    pub(crate) fn init_domain(config: &AppConfig) -> Arc<std::sync::RwLock<DomainRegistry>> {
        let root = config.data_dir.join("domain");
        std::fs::create_dir_all(root.join("inbox")).ok();
        Arc::new(std::sync::RwLock::new(
            DomainRegistry::load(&root.join("domains.json")).unwrap_or(DomainRegistry {
                domains: HashMap::new(),
                embedding_dim: 384,
            }),
        ))
    }

    pub(crate) fn init_skills(config: &AppConfig) -> Arc<SkillRegistry> {
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

    pub(crate) fn init_commands() -> Arc<SlashCommandRegistry> {
        use everevo_core::slash_command::SlashCommand;
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "List all available commands"));
        reg.register(SlashCommand::new("clear", "Clear current session history"));
        reg.register(SlashCommand::new("compact", "Trigger context compaction").with_args("topic"));
        reg.register(
            SlashCommand::new(
                "plan",
                "Enter plan mode for task planning; /plan cancel to exit",
            )
            .with_args("task"),
        );
        reg.register(SlashCommand::new("memory", "Search persistent memory").with_args("query"));
        reg.register(SlashCommand::new(
            "config",
            "Show current configuration status",
        ));
        reg.register(SlashCommand::new("tasks", "Show current task list status"));
        reg.register(SlashCommand::new(
            "doctor",
            "Run system diagnostics and show health report",
        ));
        reg.register(
            SlashCommand::new("workspace", "Set workspace directory for current session")
                .with_args("path"),
        );
        Arc::new(reg)
    }

    /// Spawn the serialized FTS5 fact-writer actor.
    ///
    /// All fact upserts flow through a single consumer task, eliminating
    /// concurrent writes to the FTS5 external-content table. This is the
    /// canonical single-writer SQLite pattern: one owned connection, writes
    /// queued via an mpsc channel, processed strictly in order.
    ///
    /// Each write retries with exponential backoff (50ms, 100ms) to absorb
    /// transient `SQLITE_BUSY` from external connections (e.g. background
    /// migrations) that briefly hold the write lock.
    ///
    /// The actor lives for the lifetime of `AppState`; it exits when the
    /// sender held by `FactManager` is dropped on shutdown.
    pub(crate) fn spawn_fact_writer(db: Database) -> FactWriteTx {
        use everevo_agent::memory::facts::FactWriteTask;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FactWriteTask>();
        tokio::spawn(async move {
            // Acquire a single connection and hold it — all upserts go through
            // this one conn, avoiding pool contention (SQLITE_BUSY_SNAPSHOT 517
            // from chat-route writes competing with re-acquired pool connections).
            let mut conn = match db.pool.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "Fact writer: cannot acquire connection — exiting");
                    return;
                }
            };
            while let Some(task) = rx.recv().await {
                for attempt in 0u32..3u32 {
                    // Sanitize content for FTS5 insert.
                    // Null bytes → "SQL logic error" (code 1).
                    // Control chars (except \n, \r, \t) also break the tokenizer.
                    // Truncation prevents porter unicode61 tokenizer overflow.
                    let content = task
                        .content
                        .replace('\0', "")
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
                        .take(500_000) // 500KB cap for FTS5 tokenizer
                        .collect::<String>();
                    if task.id.is_empty() || content.is_empty() {
                        break;
                    }
                    let result = sqlx::query(
                        "INSERT INTO facts (id, description, content, fact_type, retrieval_count, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 0, datetime('now'), datetime('now'))
                         ON CONFLICT(id) DO UPDATE SET description=?2, content=?3, fact_type=?4, updated_at=datetime('now')"
                    )
                    .bind(&task.id)
                    .bind(&task.description)
                    .bind(&content)
                    .bind(&task.fact_type)
                    .execute(&mut *conn)
                    .await;
                    match result {
                        Ok(_) => break,
                        Err(e) => {
                            if attempt < 2 {
                                let delay = std::time::Duration::from_millis(50 << attempt);
                                tracing::warn!(
                                    attempt = attempt + 1,
                                    id = %task.id,
                                    error = %e,
                                    ?delay,
                                    "Fact FTS5 upsert failed — retrying"
                                );
                                tokio::time::sleep(delay).await;
                            } else {
                                tracing::warn!(
                                    id = %task.id,
                                    error = %e,
                                    "Fact FTS5 upsert failed after 3 attempts — dropped"
                                );
                            }
                        }
                    }
                }
            }
            tracing::info!("Fact writer actor stopped");
        });
        tx
    }
}
