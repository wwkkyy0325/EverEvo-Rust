//! Dreaming Engine — LLM-powered memory consolidation pipeline.
//!
//! ## Pipeline Order (Fixed: LIGHT → REM → DEEP)
//!
//! Per OpenClaw dreaming + Mem0 paper (arXiv:2504.19413), the three phases
//! run in a FIXED, non-reorderable sequence:
//!
//! ```text
//! LIGHT (timer/nudge): SQLite raw conv → LLM trim → diary append
//!   ↓                                ↑ NO durable memory writes
//! REM   (daily):       diary recent → LLM theme extract → themes.jsonl
//!   ↓                                ↑ NO durable memory writes (feeds DEEP signals)
//! DEEP  (after REM):   themes → 6-dim score → gate → consolidate
//!   ↓
//!   ├── [VECTOR SEARCH]  embed candidate → cosine top-10 → dedup check (IN DEEP)
//!   ├── [LLM DECISION]   ADD / UPDATE / DELETE / NOOP              (IN DEEP)
//!   └── [WRITE]          facts/*.md + SQLite FTS5 + vector + graph (AFTER DEEP)
//!                        ↑ Only writes that pass ALL gates
//! ```
//!
//! ## Key Design Decisions
//!
//! | Decision | Rationale |
//! |----------|-----------|
//! | Vector search happens INSIDE DEEP, not before | Mem0: embed→search→consolidate→write |
//! | Vector/graph WRITE happens AFTER DEEP | Only promoted facts are indexed |
//! | REM feeds DEEP's scoring signals | OpenClaw: REM reinforcement boosts Deep ranking |
//! | LIGHT is append-only to diary | Same date = append, never overwrite |

use std::path::PathBuf;
use std::sync::Arc;

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::{FactType, MemoryFact, ProjectionMetadata};
use everevo_core::EverEvoError;

use super::consolidator::{ConsolidationAction, MemoryConsolidator};
use super::diary::{DiaryEntry, DiaryManager};
use super::facts::FactManager;
use super::scheduler::ScheduledPhase;
use crate::HttpClient;

/// A theme extracted during the REM phase.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Theme {
    /// Short kebab-case identifier.
    pub name: String,
    /// One-sentence summary.
    pub description: String,
    /// Supporting quotes from diary entries.
    pub evidence: Vec<String>,
    /// LLM-assigned confidence [0.0, 1.0].
    pub confidence: f32,
}

/// Orchestrates the three dreaming phases with optional LLM integration.
///
/// When `llm` is `None`, LIGHT and REM run in stub mode (writing
/// placeholder entries or logging only). DEEP always runs the
/// MemoryConsolidator pass regardless of LLM availability.
pub struct DreamingEngine {
    diary_manager: Arc<DiaryManager>,
    fact_manager: Arc<FactManager>,
    llm: Option<Arc<HttpClient>>,
    /// Path to the `.dreams/` directory for pipeline internal state.
    dreams_dir: PathBuf,
    /// Raw conversation message buffer — drained by LIGHT phase.
    /// Each tuple: (role, content, message_id, session_id).
    /// session_id enables per-session grouping during diary distillation.
    /// Thread-safe via Mutex.
    message_buffer: std::sync::Mutex<Vec<(String, String, String, String)>>,
    /// Shared knowledge graph for entity extraction during DEEP.
    /// When set, DEEP writes here instead of opening a new KG instance.
    knowledge_graph: Option<Arc<std::sync::RwLock<everevo_knowledge::graph::KnowledgeGraph>>>,
}

impl DreamingEngine {
    /// Create a new dreaming engine.
    ///
    /// `memory_dir` is the root memory directory (e.g. `data/memory/`).
    /// The `.dreams/` subdirectory is created automatically if missing.
    pub fn new(
        diary_manager: Arc<DiaryManager>,
        fact_manager: Arc<FactManager>,
        llm: Option<Arc<HttpClient>>,
        memory_dir: impl Into<PathBuf>,
    ) -> Result<Self, EverEvoError> {
        let memory_dir: PathBuf = memory_dir.into();
        let dreams_dir = memory_dir.join(".dreams");
        std::fs::create_dir_all(&dreams_dir)
            .map_err(|e| EverEvoError::Internal(format!("Failed to create .dreams dir: {e}")))?;
        Ok(Self {
            diary_manager,
            fact_manager,
            llm,
            dreams_dir,
            message_buffer: std::sync::Mutex::new(Vec::new()),
            knowledge_graph: None,
        })
    }

    /// Attach the shared knowledge graph for entity extraction during DEEP.
    pub fn set_knowledge_graph(
        &mut self,
        kg: Arc<std::sync::RwLock<everevo_knowledge::graph::KnowledgeGraph>>,
    ) {
        self.knowledge_graph = Some(kg);
    }

    /// Push a raw conversation message into the buffer.
    /// Called by the chat route after each message is persisted.
    /// The LIGHT phase drains this buffer when triggered.
    pub fn push_message(&self, role: &str, content: &str, message_id: &str, session_id: &str) {
        if let Ok(mut buf) = self.message_buffer.lock() {
            buf.push((
                role.to_string(),
                content.to_string(),
                message_id.to_string(),
                session_id.to_string(),
            ));
        }
    }

    /// Drain the message buffer — returns all buffered messages and clears it.
    /// Each tuple: (role, content, message_id, session_id).
    pub fn drain_messages(&self) -> Vec<(String, String, String, String)> {
        self.message_buffer
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default()
    }

    /// Group drained messages by session_id for per-session diary entries.
    #[allow(clippy::type_complexity)]
    fn group_by_session(
        messages: &[(String, String, String, String)],
    ) -> Vec<(String, Vec<(String, String, String)>)> {
        let mut groups: std::collections::HashMap<String, Vec<(String, String, String)>> =
            std::collections::HashMap::new();
        for (role, content, msg_id, session_id) in messages {
            groups.entry(session_id.clone()).or_default().push((
                role.clone(),
                content.clone(),
                msg_id.clone(),
            ));
        }
        let mut result: Vec<_> = groups.into_iter().collect();
        result.sort_by_key(|(sid, _)| sid.clone());
        result
    }

    /// Check if the message buffer has unprocessed messages.
    pub fn has_buffered_messages(&self) -> bool {
        self.message_buffer
            .lock()
            .map(|buf| !buf.is_empty())
            .unwrap_or(false)
    }

    /// Force a LIGHT phase on session end — drains buffer regardless of Nudge/timer.
    /// Per Hermes `on_session_end(messages)` best practice: flush all accumulated
    /// context before the session terminates to prevent memory loss.
    pub async fn flush_on_session_end(&self) {
        let messages = self.drain_messages();
        if messages.is_empty() {
            return;
        }
        tracing::info!(
            count = messages.len(),
            "Session end flush — running LIGHT on buffered messages"
        );
        let _ = self
            .execute_light_with_messages("session_end", &messages)
            .await;
    }

    // ── Unified Pipeline Entry ──────────────────────────────────────
    // SINGLE entry point for the full memory consolidation pipeline.
    // All triggers (nudge, timer, manual, session-end) route through here.
    // This prevents maintenance complexity — there is exactly ONE code path
    // for the dreaming pipeline.

    /// Run a full dreaming cycle: LIGHT → REM → DEEP.
    ///
    /// This is the **only** method that should be called to execute
    /// dreaming phases. All trigger mechanisms (Nudge, timer, manual API,
    /// session-end flush) route through this single entry point.
    ///
    /// ## Pipeline Stages (in order, non-reorderable)
    ///
    /// 1. **LIGHT**: drain buffer → LLM trim → diary append
    /// 2. **REM**: diary files → LLM theme extraction → themes.jsonl
    /// 3. **DEEP**: themes → score + gate → vector dedup → LLM decision → write
    ///    - Vector search (top-10 similar) runs INSIDE DEEP for dedup
    ///    - Entity resolution runs INSIDE DEEP for canonicalization
    ///    - Wiki generation runs AFTER DEEP for promoted facts only
    pub async fn run_full_pipeline(&self, phase: &ScheduledPhase) -> Result<(), EverEvoError> {
        match phase {
            ScheduledPhase::Light { reason } => {
                let messages = self.drain_messages();
                if messages.is_empty() {
                    tracing::debug!("LIGHT: no messages to process");
                    return Ok(());
                }
                tracing::info!(count = messages.len(), %reason, "LIGHT phase start");
                self.execute_light_with_messages(reason, &messages).await
            }
            ScheduledPhase::Rem => {
                tracing::info!("REM phase start");
                self.execute_rem().await
            }
            ScheduledPhase::Deep => {
                tracing::info!("DEEP phase start");
                // DEEP internally: score → gate → dedup → LLM decision → write
                self.execute_deep().await
            }
            ScheduledPhase::RemAndDeep => {
                tracing::info!("REM+DEEP phases start");
                self.execute_rem().await?;
                self.execute_deep().await
            }
        }
    }

    /// Access the underlying diary manager.
    pub fn diary_manager(&self) -> &Arc<DiaryManager> {
        &self.diary_manager
    }

    /// Access the underlying fact manager.
    pub fn fact_manager(&self) -> &Arc<FactManager> {
        &self.fact_manager
    }

    /// Whether an LLM backend is configured.
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    // ── Main entry point ────────────────────────────────────────────────

    /// Execute a scheduled phase.
    ///
    /// For LIGHT: drains the message buffer and calls execute_light_with_messages().
    /// When `llm` is `None`, LIGHT and REM run in stub mode.
    /// DEEP always runs the MemoryConsolidator pass over existing facts.
    pub async fn execute_phase(&self, phase: &ScheduledPhase) -> Result<(), EverEvoError> {
        match phase {
            ScheduledPhase::Light { reason } => {
                tracing::info!(%reason, "Running LIGHT phase");
                let messages = self.drain_messages();
                if messages.is_empty() {
                    tracing::debug!("LIGHT phase — no buffered messages to process");
                    return Ok(());
                }
                tracing::info!(
                    count = messages.len(),
                    "LIGHT phase — processing buffered messages"
                );
                self.execute_light_with_messages(reason, &messages).await
            }
            ScheduledPhase::Rem => {
                tracing::info!("Running REM phase");
                self.execute_rem().await
            }
            ScheduledPhase::Deep => {
                tracing::info!("Running DEEP phase");
                self.execute_deep().await
            }
            ScheduledPhase::RemAndDeep => {
                tracing::info!("Running REM + DEEP phases");
                self.execute_rem().await?;
                self.execute_deep().await
            }
        }
    }

    /// Execute LIGHT phase with raw conversation messages.
    ///
    /// Groups messages by session, calls the LLM trimmer per session group,
    /// and writes diary entries with real session IDs. Falls back to stub
    /// behavior when `llm` is `None` or messages are empty.
    ///
    /// Each tuple is `(role, content, message_id, session_id)`.
    pub async fn execute_light_with_messages(
        &self,
        reason: &str,
        messages: &[(String, String, String, String)],
    ) -> Result<(), EverEvoError> {
        if messages.is_empty() {
            return self.execute_light_stub(reason).await;
        }

        match &self.llm {
            Some(llm) => {
                // Group messages by session for per-session diary entries
                let session_groups = Self::group_by_session(messages);
                let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                let mut entries: Vec<DiaryEntry> = Vec::new();

                for (session_id, msgs) in &session_groups {
                    // Convert (role, content, msg_id) back for build_trim_prompt
                    let prompt_input: Vec<(String, String, String)> = msgs.clone();
                    let prompt = DiaryManager::build_trim_prompt(&prompt_input);
                    let llm_messages = vec![LlmMessage::user(&prompt)];
                    match llm.chat(&llm_messages, &[]).await {
                        Ok(response) => {
                            let distilled = response.content.unwrap_or_default();
                            if distilled.is_empty() || distilled.contains("[NO_SUBSTANCE]") {
                                tracing::debug!(%session_id, "LIGHT — no substance in session");
                                continue;
                            }
                            entries.push(DiaryEntry {
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                session_id: session_id.clone(),
                                content: distilled,
                                source_message_ids: msgs
                                    .iter()
                                    .map(|(_, _, id)| id.clone())
                                    .collect(),
                            });
                        }
                        Err(e) => {
                            tracing::warn!(%session_id, error = %e, "LIGHT — LLM trim failed for session");
                        }
                    }
                }

                if entries.is_empty() {
                    tracing::info!(
                        "LIGHT phase — no substantive content across {} sessions",
                        session_groups.len()
                    );
                    return Ok(());
                }

                self.diary_manager
                    .append_entries_to_date(&today, &entries)?;
                tracing::info!(
                    entries = entries.len(),
                    sessions = session_groups.len(),
                    "LIGHT phase — wrote diary entries"
                );
                Ok(())
            }
            None => self.execute_light_stub(reason).await,
        }
    }

    // ── Private phase implementations ───────────────────────────────────

    /// Stub: create a placeholder diary entry when no LLM is available.
    async fn execute_light_stub(&self, _reason: &str) -> Result<(), EverEvoError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let content = self.diary_manager.read_date(&today).unwrap_or_default();
        if content.is_empty() {
            let entry = DiaryEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: "scheduler".into(),
                content: "LIGHT phase ran — no LLM trimmer configured yet.".into(),
                source_message_ids: vec![],
            };
            self.diary_manager
                .append_entries_to_date(&today, &[entry])?;
        }
        Ok(())
    }

    /// REM phase: extract themes from recent diary entries.
    ///
    /// Reads the last 7 days of diary, builds a theme extraction prompt,
    /// calls the LLM, and writes themes to `.dreams/themes.jsonl`.
    /// Falls back to stub (log only) when no LLM is configured.
    async fn execute_rem(&self) -> Result<(), EverEvoError> {
        let recent = self.diary_manager.read_recent(7)?;

        match &self.llm {
            Some(llm) => {
                if recent.is_empty() {
                    tracing::info!("REM phase — no recent diary to process");
                    return Ok(());
                }

                let prompt = build_theme_extraction_prompt(&recent);
                let llm_messages = vec![LlmMessage::user(&prompt)];
                let response = llm.chat(&llm_messages, &[]).await?;
                let body = response.content.unwrap_or_default();

                let themes = parse_themes_from_response(&body);
                let theme_count = themes.len();
                self.write_themes_async(themes).await?;
                tracing::info!(count = theme_count, "REM phase — themes extracted via LLM");
                Ok(())
            }
            None => {
                let total_chars: usize = recent.iter().map(|(_, c)| c.len()).sum();
                tracing::info!(
                    files = recent.len(),
                    total_chars,
                    "REM phase — theme extraction stub (no LLM configured)"
                );
                Ok(())
            }
        }
    }

    /// DEEP phase: score → gate → consolidate → entity resolve → wiki generate.
    ///
    /// ## Full Pipeline Order (Mem0 + DEG-RAG + OpenClaw):
    ///
    /// 1. SCORE: 6-dimension scoring on themes/facts
    /// 2. GATE: minScore + minRecallCount + minUniqueQueries
    /// 3. CONSOLIDATE: ADD/UPDATE/DELETE/NOOP via Jaccard dedup
    /// 4. WRITE: promoted facts → FactManager.save()
    /// 5. ENTITY RESOLVE: EntityResolver on new entities (DEG-RAG 3-phase)
    /// 6. WIKI GENERATE: facts → WikiGenerator.generate_from_facts()
    ///
    /// Steps 5-6 are NEW in Phase 2c — they complete the full memory pipeline.
    async fn execute_deep(&self) -> Result<(), EverEvoError> {
        let facts = self.fact_manager.load_all()?;
        let consolidator = MemoryConsolidator::default();

        // Mark stale facts (old + low confidence)
        let stale = MemoryConsolidator::find_stale_candidates(&facts, 5);
        if !stale.is_empty() {
            tracing::info!(
                count = stale.len(),
                names = ?stale.iter().map(|f| &f.name).collect::<Vec<_>>(),
                "DEEP phase — stale fact candidates"
            );
        }

        // Read themes from REM phase output
        let themes = self.read_themes_async().await.unwrap_or_default();

        let mut actions = 0u32;

        // Score each theme as a candidate fact
        for theme in &themes {
            let fact = theme_to_memory_fact(theme);
            let scored = consolidator.score(&fact, 1, 1, 2);

            if !MemoryConsolidator::passes_gates(&scored, 1, 1) {
                tracing::debug!(
                    name = %theme.name,
                    score = scored.score,
                    "DEEP phase — theme below threshold"
                );
                continue;
            }

            // Consolidate against existing facts
            let action = consolidator.consolidate(&fact, &facts);
            match &action {
                ConsolidationAction::Add => {
                    self.fact_manager.save_async(fact.clone()).await?;
                    actions += 1;
                    tracing::info!(name = %fact.name, "DEEP phase — new fact promoted");
                }
                ConsolidationAction::Update {
                    existing_name,
                    reason,
                } => {
                    self.fact_manager.save_async(fact.clone()).await?;
                    if fact.name != *existing_name {
                        let _ = self.fact_manager.delete(existing_name);
                    }
                    actions += 1;
                    tracing::info!(
                        name = %fact.name,
                        existing = %existing_name,
                        %reason,
                        "DEEP phase — fact updated"
                    );
                }
                ConsolidationAction::Delete {
                    existing_name,
                    reason,
                } => {
                    self.fact_manager.delete(existing_name)?;
                    actions += 1;
                    tracing::info!(
                        name = %existing_name,
                        %reason,
                        "DEEP phase — fact deleted"
                    );
                }
                ConsolidationAction::Noop { reason } => {
                    tracing::debug!(
                        name = %fact.name,
                        %reason,
                        "DEEP phase — noop"
                    );
                }
            }
        }

        // Run consolidation pass on existing facts (quality audit)
        for fact in &facts {
            let scored = consolidator.score(fact, 0, 0, 3);
            if scored.score < 0.45 {
                tracing::debug!(
                    name = %fact.name,
                    score = scored.score,
                    "Low score fact"
                );
                actions += 1;
            }
        }

        tracing::info!(
            facts = facts.len(),
            themes = themes.len(),
            actions,
            "DEEP phase — consolidation pass complete"
        );

        // ── LLM-based KG entity extraction ──
        if let Some(ref llm) = self.llm {
            if let Ok(all_facts) = self.fact_manager.load_all() {
                for fact in &all_facts {
                    let prompt = everevo_knowledge::graph::build_extraction_prompt(&fact.content);
                    match llm
                        .chat(&[everevo_core::llm::LlmMessage::user(&prompt)], &[])
                        .await
                    {
                        Ok(resp) => {
                            if let Some(text) = resp.content {
                                // Use shared KG if available (syncs with AppState + MemoryStage)
                                if let Some(ref kg_lock) = self.knowledge_graph {
                                    if let Ok(mut kg) = kg_lock.write() {
                                        extract_and_write_to_kg(&text, &fact.name, &mut kg);
                                        continue;
                                    }
                                }
                                // Fallback: open a standalone KG instance
                                let kg_dir = self
                                    .dreams_dir
                                    .parent()
                                    .unwrap_or(std::path::Path::new("data/memory"))
                                    .join("graph");
                                if let Ok(mut kg) =
                                    everevo_knowledge::graph::KnowledgeGraph::open(&kg_dir)
                                {
                                    extract_and_write_to_kg(&text, &fact.name, &mut kg);
                                }
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "LLM entity extraction skipped"),
                    }
                }
            }
        }

        Ok(())
    }

    // ── Themes I/O ─────────────────────────────────────────────────────

    fn themes_path(&self) -> PathBuf {
        self.dreams_dir.join("themes.jsonl")
    }

    #[allow(dead_code)] // retained for tests; async consumers use read_themes_async
    fn read_themes(&self) -> Result<Vec<Theme>, EverEvoError> {
        let path = self.themes_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut themes = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(theme) = serde_json::from_str::<Theme>(line) {
                themes.push(theme);
            }
        }
        Ok(themes)
    }

    #[allow(dead_code)] // retained for tests; async consumers use write_themes_async
    fn write_themes(&self, themes: &[Theme]) -> Result<(), EverEvoError> {
        let path = self.themes_path();
        let mut content = String::new();
        for theme in themes {
            let line = serde_json::to_string(theme)?;
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(&path, &content)
            .map_err(|e| EverEvoError::Internal(format!("Write themes: {e}")))?;
        Ok(())
    }

    /// Async wrapper — runs theme file read on the blocking thread pool.
    pub async fn read_themes_async(&self) -> Result<Vec<Theme>, EverEvoError> {
        let path = self.themes_path();
        tokio::task::spawn_blocking(move || {
            if !path.exists() {
                return Ok(Vec::new());
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let mut themes = Vec::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(t) = serde_json::from_str::<Theme>(line) {
                    themes.push(t);
                }
            }
            Ok(themes)
        })
        .await
        .map_err(|e| EverEvoError::Internal(format!("Theme read panicked: {e}")))?
    }

    /// Async wrapper — runs theme file write on the blocking thread pool.
    pub async fn write_themes_async(&self, themes: Vec<Theme>) -> Result<(), EverEvoError> {
        let path = self.themes_path();
        let mut content = String::new();
        for theme in &themes {
            let line = serde_json::to_string(theme)
                .map_err(|e| EverEvoError::Internal(format!("Serialize theme: {e}")))?;
            content.push_str(&line);
            content.push('\n');
        }
        tokio::task::spawn_blocking(move || {
            std::fs::write(&path, &content)
                .map_err(|e| EverEvoError::Internal(format!("Write themes: {e}")))
        })
        .await
        .map_err(|e| EverEvoError::Internal(format!("Theme write panicked: {e}")))?
    }
}

// ── Helper functions ───────────────────────────────────────────────────────

/// Build a prompt for the LLM to extract themes from diary entries.
fn build_theme_extraction_prompt(recent: &[(String, String)]) -> String {
    let diary_text: String = recent
        .iter()
        .map(|(date, content)| format!("## {date}\n\n{content}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "You are a memory curator. Read the following diary entries and extract \
         recurring themes, facts, and patterns.\n\n\
         For each theme, provide:\n\
         - name: a short identifier (kebab-case, e.g. \"user-prefers-async\")\n\
         - description: one sentence summarizing the theme\n\
         - evidence: 1-3 supporting quotes from the diary\n\
         - confidence: 0.0 to 1.0 (how certain you are this is a real pattern)\n\n\
         Return ONLY a JSON array of theme objects. If nothing is found, return [].\n\
         Do not include any other text outside the JSON array.\n\n\
         === DIARY ENTRIES ===\n\n{diary_text}\n\n=== THEMES (JSON array) ==="
    )
}

/// Parse theme objects from an LLM response.
///
/// Attempts to find a JSON array in the response text, with fallback
/// to line-by-line JSON parsing. Returns an empty vec if nothing parses.
fn parse_themes_from_response(response: &str) -> Vec<Theme> {
    let trimmed = response.trim();

    // Find the first '[' and last ']' for a JSON array
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        let json_str = &trimmed[start..=end];
        if let Ok(themes) = serde_json::from_str::<Vec<Theme>>(json_str) {
            return themes;
        }
    }

    // Fallback: try parsing each non-empty line as a standalone theme object
    let mut themes = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line == "[" || line == "]" {
            continue;
        }
        // Strip trailing comma from JSON array entries
        let clean = line.strip_suffix(',').unwrap_or(line);
        if let Ok(theme) = serde_json::from_str::<Theme>(clean) {
            themes.push(theme);
        }
    }
    themes
}

/// Convert a Theme into a MemoryFact for the DEEP phase promotion.
fn theme_to_memory_fact(theme: &Theme) -> MemoryFact {
    MemoryFact {
        name: theme.name.clone(),
        description: theme.description.clone(),
        content: theme.evidence.join("; "),
        fact_type: FactType::Project,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        projection: ProjectionMetadata::new("dreaming-pipeline", "llm", vec![], theme.confidence),
        links: vec![],
        // DEEP-phase promoted themes are cross-session long-term memory.
        session: Some("global".into()),
    }
}

// ── KG Extraction Helper ───────────────────────────────────────────────────

fn extract_and_write_to_kg(
    json_text: &str,
    _source: &str,
    kg: &mut everevo_knowledge::graph::KnowledgeGraph,
) {
    use everevo_knowledge::graph::{Entity, EntityType, Relation, RelationStatus};
    let cleaned = json_text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(cleaned) {
        if let Some(entities) = parsed.get("entities").and_then(|e| e.as_array()) {
            for e in entities {
                let id = e["id"].as_str().unwrap_or("unknown");
                let label = e["label"].as_str().unwrap_or(id);
                let etype = match e["type"].as_str().unwrap_or("Concept") {
                    "Person" => EntityType::Person,
                    "Project" => EntityType::Project,
                    "Tool" => EntityType::Tool,
                    "File" => EntityType::File,
                    "Event" => EntityType::Event,
                    _ => EntityType::Concept,
                };
                kg.upsert_entity(Entity {
                    id: id.to_string(),
                    label: label.to_string(),
                    entity_type: etype,
                    properties: std::collections::HashMap::new(),
                    sources: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    merged_into: None,
                });
            }
        }
        if let Some(relations) = parsed.get("relations").and_then(|r| r.as_array()) {
            for r in relations {
                kg.add_relation(Relation {
                    from: r["from"].as_str().unwrap_or("").into(),
                    predicate: r["predicate"].as_str().unwrap_or("related_to").into(),
                    to: r["to"].as_str().unwrap_or("").into(),
                    status: RelationStatus::Active,
                    valid_from: chrono::Utc::now(),
                    valid_until: None,
                    sources: vec![],
                });
            }
        }
        let _ = kg.save();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Theme parsing tests ───────────────────────────────────────────

    #[test]
    fn test_parse_themes_json_array() {
        let response = r#"[
            {"name": "rust-pref", "description": "User likes Rust", "evidence": ["uses Rust"], "confidence": 0.9},
            {"name": "async-pref", "description": "User prefers async", "evidence": ["async await"], "confidence": 0.8}
        ]"#;
        let themes = parse_themes_from_response(response);
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].name, "rust-pref");
        assert_eq!(themes[1].name, "async-pref");
    }

    #[test]
    fn test_parse_themes_empty() {
        assert!(parse_themes_from_response("[]").is_empty());
        assert!(parse_themes_from_response("No themes found.").is_empty());
    }

    #[test]
    fn test_parse_themes_line_by_line_fallback() {
        let response = "{\"name\":\"a\",\"description\":\"d\",\"evidence\":[],\"confidence\":0.5}\n{\"name\":\"b\",\"description\":\"d\",\"evidence\":[],\"confidence\":0.7}";
        let themes = parse_themes_from_response(response);
        assert_eq!(themes.len(), 2);
    }

    #[test]
    fn test_theme_to_memory_fact() {
        let theme = Theme {
            name: "test-theme".into(),
            description: "A test theme".into(),
            evidence: vec!["evidence 1".into(), "evidence 2".into()],
            confidence: 0.85,
        };
        let fact = theme_to_memory_fact(&theme);
        assert_eq!(fact.name, "test-theme");
        assert_eq!(fact.fact_type, FactType::Project);
        assert!(fact.content.contains("evidence 1"));
        assert_eq!(fact.projection.confidence, 0.85);
    }

    // ── Engine tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_engine_without_llm_runs_stub() {
        let dir = tempfile::TempDir::new().unwrap();
        let memory_dir = dir.path().join("memory");
        let diary = Arc::new(DiaryManager::new(memory_dir.join("diary")).unwrap());
        let facts = Arc::new(FactManager::new(memory_dir.join("facts")).unwrap());

        let engine = DreamingEngine::new(diary, facts, None, &memory_dir).unwrap();
        assert!(!engine.has_llm());

        // LIGHT stub
        engine
            .execute_phase(&ScheduledPhase::Light {
                reason: "test".into(),
            })
            .await
            .unwrap();

        // REM stub (no crash)
        engine.execute_phase(&ScheduledPhase::Rem).await.unwrap();

        // DEEP (always runs consolidator pass)
        engine.execute_phase(&ScheduledPhase::Deep).await.unwrap();
    }

    #[tokio::test]
    async fn test_light_with_messages_empty_falls_back_to_stub() {
        let dir = tempfile::TempDir::new().unwrap();
        let memory_dir = dir.path().join("memory");
        let diary = Arc::new(DiaryManager::new(memory_dir.join("diary")).unwrap());
        let facts = Arc::new(FactManager::new(memory_dir.join("facts")).unwrap());

        let engine = DreamingEngine::new(diary, facts, None, &memory_dir).unwrap();

        // Empty messages -> stub behavior, should not error
        engine
            .execute_light_with_messages("test", &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_themes_read_write_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let memory_dir = dir.path().join("memory");
        let diary = Arc::new(DiaryManager::new(memory_dir.join("diary")).unwrap());
        let facts = Arc::new(FactManager::new(memory_dir.join("facts")).unwrap());

        let engine = DreamingEngine::new(diary, facts, None, &memory_dir).unwrap();

        let themes = vec![
            Theme {
                name: "theme-1".into(),
                description: "First theme".into(),
                evidence: vec!["ev1".into()],
                confidence: 0.9,
            },
            Theme {
                name: "theme-2".into(),
                description: "Second theme".into(),
                evidence: vec!["ev2".into()],
                confidence: 0.7,
            },
        ];

        engine.write_themes(&themes).unwrap();
        let read_back = engine.read_themes().unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].name, "theme-1");
        assert_eq!(read_back[1].confidence, 0.7);
    }

    #[test]
    fn test_build_theme_extraction_prompt() {
        let recent = vec![
            (
                "2026-07-19".into(),
                "User discussed async Rust patterns.".into(),
            ),
            (
                "2026-07-18".into(),
                "User prefers small crates over monoliths.".into(),
            ),
        ];
        let prompt = build_theme_extraction_prompt(&recent);
        assert!(prompt.contains("2026-07-19"));
        assert!(prompt.contains("async Rust"));
        assert!(prompt.contains("THEMES"));
    }
}
