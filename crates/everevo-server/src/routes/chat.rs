//! Chat endpoint — session-aware SSE streaming with context pipeline.
//!
//! Flow:
//! 1. Resolve or create session
//! 2. Load conversation history from DB
//! 3. Assemble context via ContextPipeline (system prompt + history + user msg)
//! 4. Persist user message
//! 5. Stream LLM tokens via SSE
//! 6. Persist assistant response
//! 7. Send `Done` event with session_id + message_id

use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::post,
    Json, Router,
};
use chrono::Utc;
use everevo_core::context::{ContextBuildContext, default_pipeline};
use everevo_core::llm::{LlmMessage, LlmRole};
use everevo_core::types::ChatRequest;
use everevo_db::models::MessageRow;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::app_state::{AppState, ConfirmationNotification, PendingConfirmation};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/chat", post(handler))
}

async fn handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        if let Err(e) = handle_chat(state, req, &tx).await {
            let _ = tx
                .send(Ok(Event::default().event("error").data(e.to_string())))
                .await;
        }
    });

    Sse::new(ReceiverStream::new(rx))
}

async fn handle_chat(
    state: Arc<AppState>,
    req: ChatRequest,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), String> {
    // ── 1. Resolve or create session ──────────────────────────────────
    let session_id = match req.session_id {
        Some(id) => {
            state
                .db
                .get_session(id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Session not found".to_string())?;
            // Restore sandbox if it doesn't exist (e.g., after restart)
            if !state.sandboxes.read().await.contains_key(&id) {
                let _ = state.create_sandbox(id, resolve_permission(&state.config.default_permission_level)).await;
            }
            id
        }
        None => {
            let title = truncate_for_title(&req.message);
            let row = state
                .db
                .create_session(&title)
                .await
                .map_err(|e| e.to_string())?;
            let _ = state.create_sandbox(row.id, resolve_permission(&state.config.default_permission_level)).await;
            row.id
        }
    };

    // ── 1.5 Start telemetry trace for this session ──────────────────
    let trace = state.telemetry.start_trace(session_id);
    let trace_id = trace.as_ref().map(|t| t.trace_id);

    // ── 2. Load conversation history ──────────────────────────────────
    let db_messages = state
        .db
        .get_messages(session_id, Some(20))
        .await
        .map_err(|e| e.to_string())?;

    // Filter out tool-call and tool-result messages from DB history.
    // DeepSeek V4 / Anthropic protocol requires every tool_use to have a
    // matching tool_result in the next message. DB history may have
    // incomplete pairs from previous sessions — skip them entirely.
    let history: Vec<LlmMessage> = db_messages
        .iter()
        .filter(|m| m.role != "tool" && m.tool_calls.is_none())
        .map(|m| db_message_to_llm(m))
        .collect();

    // ── 3. Build context via pipeline ─────────────────────────────────
    let (shell_name, permission_level, trusted_paths) = {
        let sandboxes = state.sandboxes.read().await;
        sandboxes.get(&session_id).map(|sb| {
            (sb.engine().shell_name().to_string(), sb.permission_level().label().to_string(), sb.trusted_paths())
        }).unwrap_or_default()
    };
    let tool_count = 4; // shell + download + bootstrap + memory
    let ctx = ContextBuildContext {
        user_message: req.message.clone(),
        session_id: Some(session_id),
        session_title: None,
        history,
        history_tokens: 0,
        max_context_tokens: state.config.max_context_tokens,
        shell_name: Some(shell_name.clone()),
        permission_level: Some(permission_level),
        trusted_paths,
        tool_count,
    };
    let persona_profile_path = state.config.data_dir
        .join("memory")
        .join("persona")
        .join("profile.json");
    let memory_stage = {
        let stage = everevo_agent::memory::MemoryStage::new(state.fact_manager.clone());
        if let Some(tid) = trace_id {
            stage.with_telemetry(state.telemetry.clone(), tid)
        } else {
            stage
        }
    };
    let domain_root = state.config.data_dir.join("domain");
    let domain_stage = everevo_agent::DomainKnowledgeStage::new(&domain_root)
        .with_max_docs(3);

    // Parent work dir for sub-agent path inheritance.
    let parent_work_dir = state.sandboxes.read().await
        .get(&session_id)
        .map(|sb| sb.work_dir().clone());

    // Build sub-agent context BEFORE the pipeline consumes domain_stage.
    let shell = shell_name.clone();
    let sub_ctx = everevo_agent::subagent_context::assemble_subagent_context(
        &req.message,
        None,
        Some(&domain_stage),
        parent_work_dir,
        None,
        &shell,
        &["shell".into(), "memory".into()],
    ).await;

    let pipeline = default_pipeline()
        .with_stage(everevo_agent::persona::PersonaStage::new(persona_profile_path))
        .with_stage(everevo_agent::skill::SkillStage::new(state.skill_registry.clone()))
        .with_stage(memory_stage)
        .with_stage(domain_stage);
    let messages = pipeline.assemble(&ctx);

    // ── 4. Persist user message ──────────────────────────────────────
    let user_msg = MessageRow::new(
        session_id,
        "user",
        req.message.clone(),
        None,
        None,
    );
    state
        .db
        .add_message(&user_msg)
        .await
        .map_err(|e| format!("Failed to save user message: {e}"))?;

    // Feed raw message into the dreaming engine's buffer
    state.dreaming_engine.push_message("user", &req.message, &user_msg.id.to_string());

    // Bump session updated_at
    let _ = state
        .db
        .update_session_title(session_id, &truncate_for_title(&req.message))
        .await;

    // ── 5. Get LLM client ────────────────────────────────────────────
    let guard = state.llm.read().await;
    let client = guard
        .get("primary")
        .and_then(|c| c.as_ref())
        .or_else(|| guard.values().find_map(|c| c.as_ref()))
        .cloned();
    drop(guard);

    let client = client.ok_or_else(|| "未配置 LLM".to_string())?;

    // ── 6. Build tool registry for this session ──────────────────────
    // Create notification channel for confirmation flow.
    // The SSE stream listens on notif_rx while the tool sends on notif_tx.
    let (notif_tx, mut notif_rx) = mpsc::unbounded_channel::<ConfirmationNotification>();

    let mut registry = everevo_core::tool::ToolRegistry::new();
    {
        let sandboxes = state.sandboxes.read().await;
        if let Some(sb) = sandboxes.get(&session_id) {
            let provider = sb.provider();
            let work_dir = sb.work_dir().clone();
            let sandbox = Arc::new(SandboxedShellTool {
                inner: provider,
                work_dir,
                session_id,
                confirmations: state.confirmations.clone(),
                notif_tx,
            });
            registry.register(sandbox);
        }
    }
    // Global tools (download + bootstrap) — always available
    registry.register(Arc::new(everevo_agent::tools::builtins::DownloadTool::new(
        state.downloader.clone(),
    )));
    registry.register(Arc::new(everevo_agent::tools::builtins::BootstrapTool::new(
        state.bootstrap.clone(),
    )));
    registry.register(Arc::new(everevo_agent::tools::builtins::MemoryTool::new(
        state.fact_manager.clone(),
    )));
    // Task tool — LLM decides when to spawn subagents (Claude Code pattern).
    // Needs the base registry (shell+memory) for subagent tool inheritance.
    // Created from partial registry, then registered last.
    let mut base_for_task = everevo_core::tool::ToolRegistry::new();
    if let Some(shell) = registry.get("shell") { base_for_task.register(Arc::clone(shell)); }
    if let Some(memory) = registry.get("memory") { base_for_task.register(Arc::clone(memory)); }

    let task_tool = everevo_agent::tools::builtins::TaskTool::new(
        Arc::new(state.config.data_dir.join("sandbox")),
        Arc::new(base_for_task),
        Some(Arc::clone(&client)),
    );
    let pending_subagents = task_tool.pending.clone();
    let subagent_rx = task_tool.take_receiver(); // AgentLoop drains this each turn
    // Set the pre-built sub-agent context on the TaskTool.
    *task_tool.subagent_ctx.write().unwrap() = sub_ctx;
    let profile_path = state.config.data_dir.join("memory").join("persona").join("profile.json");
    if let Ok(content) = std::fs::read_to_string(&profile_path) {
        if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(injection) = profile.get("system_prompt_injection").and_then(|v| v.as_str()) {
                task_tool.set_persona(injection.to_string());
            }
        }
    }
    registry.register(Arc::new(task_tool));
    let tools = Arc::new(registry);
    tracing::info!(tool_count = tools.len(), "Agent tools ready");

    // ── 7. Run Agent Loop with Confirmation Support ────────────────
    // We use tokio::select! to listen on TWO channels simultaneously:
    //   1. agent_rx  — agent events (thinking, text, tool calls, done, error)
    //   2. notif_rx  — confirmation notifications from the shell tool
    //
    // When the shell tool needs user confirmation, it sends a notification
    // on notif_tx and BLOCKS on a oneshot. The SSE stream forwards the
    // notification to the frontend as a "confirmation_required" event.
    // The user clicks Allow/Deny → /api/sandbox/confirm resolves the
    // oneshot → the tool unblocks and continues. The LLM never sees any
    // of this — it's transparent, just like Claude Code.
    let agent = everevo_agent::AgentLoop::new()
        .with_subagent_channel(subagent_rx)  // non-blocking task tool results
        .with_pending_subagents(pending_subagents); // block Done while sub-agents run
    let mut agent_rx = agent.run(client, tools, messages, None).await;

    let mut full_response = String::new();
    let assistant_id = Uuid::new_v4();
    // Accumulate tool calls within the current turn for DB persistence.
    let mut turn_tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut turn_tool_results: Vec<(String, String, bool)> = Vec::new(); // (id, content, is_error)

    loop {
        tokio::select! {
            // ── Agent events (primary channel) ──────────────────────
            Some(event) = agent_rx.recv() => {
                match event {
                    everevo_agent::AgentEvent::Thinking(t) => {
                        if tx.send(Ok(Event::default().event("thinking").data(t))).await.is_err() { break; }
                    }
                    everevo_agent::AgentEvent::TextDelta(t) => {
                        full_response.push_str(&t);
                        if tx.send(Ok(Event::default().event("token").data(t))).await.is_err() { break; }
                    }
                    everevo_agent::AgentEvent::ToolCallStart { id, name, arguments } => {
                        turn_tool_calls.push(serde_json::json!({
                            "id": id, "name": name, "arguments": arguments,
                        }));
                        let _ = tx.send(Ok(Event::default().event("tool_start").data(
                            serde_json::json!({"id": id, "name": name, "arguments": arguments}).to_string(),
                        ))).await;
                    }
                    everevo_agent::AgentEvent::ToolCallEnd { id, name, content, is_error } => {
                        let id_c = id.clone();
                        turn_tool_results.push((id, content.clone(), is_error));
                        let _ = tx.send(Ok(Event::default().event("tool_end").data(
                            serde_json::json!({"id": id_c, "name": name, "content": content, "is_error": is_error}).to_string(),
                        ))).await;
                    }
                    everevo_agent::AgentEvent::ConfirmationNeeded { command, reason } => {
                        let _ = tx.send(Ok(Event::default().event("confirmation_required").data(
                            serde_json::json!({"command": command, "reason": reason}).to_string(),
                        ))).await;
                    }
                    everevo_agent::AgentEvent::TurnComplete => {
                        // Persist tool calls + results for this turn so they survive
                        // SSE disconnects and server restarts.
                        if !turn_tool_calls.is_empty() {
                            let tc_json = serde_json::to_string(&turn_tool_calls).unwrap_or_default();
                            let _ = state.db.add_message(&MessageRow::new(
                                session_id, "assistant", "", Some(tc_json), None,
                            )).await;
                            for (tc_id, tc_content, _tc_err) in &turn_tool_results {
                                let _ = state.db.add_message(&MessageRow::new(
                                    session_id, "tool", tc_content, None, Some(tc_id.clone()),
                                )).await;
                            }
                            turn_tool_calls.clear();
                            turn_tool_results.clear();
                        }
                        state.scheduler.increment_turn();
                    }
                    everevo_agent::AgentEvent::Done { final_text } => {
                        full_response = final_text;
                        break;
                    }
                    everevo_agent::AgentEvent::Error { message } => {
                        let _ = tx.send(Ok(Event::default().event("error").data(message))).await;
                        break;
                    }
                }
            }

            // ── Confirmation notifications (shell tool → frontend) ──
            Some(notif) = notif_rx.recv() => {
                tracing::info!(
                    session_id = %notif.session_id,
                    command = %notif.command,
                    "Sending confirmation_required to frontend"
                );
                let payload = serde_json::json!({
                    "session_id": notif.session_id.to_string(),
                    "command": notif.command,
                    "reason": notif.reason,
                });
                let _ = tx.send(Ok(Event::default()
                    .event("confirmation_required")
                    .data(payload.to_string())
                )).await;
                // The tool is blocked on its oneshot — we don't need to
                // wait here. The /api/sandbox/confirm endpoint will
                // resolve it when the user clicks.
            }
        }
    }

    // ── 8. Persist assistant response ─────────────────────────────────
    if !full_response.is_empty() {
        let content_hash = everevo_db::models::sha256_hash(&full_response);
        let assistant_msg = MessageRow {
            id: assistant_id,
            session_id,
            role: "assistant".into(),
            content: full_response.clone(),
            content_hash,
            tool_calls: None,
            tool_call_id: None,
            created_at: Utc::now(),
        };
        let _ = state.db.add_message(&assistant_msg).await;
        state.dreaming_engine.push_message("assistant", &full_response, &assistant_id.to_string());
    }

    // ── 9. Flush audit trail ───────────────────────────────────────
    if let Some(sb) = state.sandboxes.read().await.get(&session_id) {
        sb.flush_audit();
    }

    // ── 10. Done ─────────────────────────────────────────────────────
    let done_payload = serde_json::json!({
        "session_id": session_id,
        "message_id": assistant_id,
    });
    let _ = tx
        .send(Ok(Event::default().event("done").data(done_payload.to_string())))
        .await;

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────

fn truncate_for_title(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    if first_line.chars().count() > 60 {
        first_line.chars().take(57).chain("...".chars()).collect()
    } else {
        first_line.to_string()
    }
}

/// Wraps a sandbox to force all commands into the session work directory.
/// Also handles the confirmation flow: when the sandbox requires user
/// confirmation, this tool blocks on a oneshot channel until the user
/// responds via the `/api/sandbox/confirm` endpoint.
struct SandboxedShellTool {
    inner: Arc<dyn everevo_core::sandbox::SandboxProvider>,
    work_dir: std::path::PathBuf,
    session_id: Uuid,
    /// Shared pending confirmations map — the confirm endpoint resolves these.
    confirmations: Arc<RwLock<std::collections::HashMap<Uuid, PendingConfirmation>>>,
    /// Channel to notify the SSE stream about a pending confirmation.
    notif_tx: mpsc::UnboundedSender<ConfirmationNotification>,
}

#[async_trait::async_trait]
impl everevo_core::tool::Tool for SandboxedShellTool {
    fn name(&self) -> &str { "shell" }
    fn description(&self) -> &str {
        "Execute a shell command in an isolated sandbox. Use RELATIVE paths (e.g., ./file.txt)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command. Use relative paths." },
                "timeout_secs": { "type": "integer", "description": "Timeout (default: 30, max: 300)", "default": 30 }
            },
            "required": ["command"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel { everevo_core::types::RiskLevel::Medium }
    async fn execute(&self, params: serde_json::Value) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        let command = params["command"].as_str()
            .ok_or_else(|| everevo_core::EverEvoError::InvalidInput("command is required".into()))?;
        let timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30).min(300);
        let confirmed = params.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false);

        let config = everevo_core::sandbox::ExecutionConfig::new(command)
            .with_timeout(timeout_secs)
            .with_working_dir(self.work_dir.clone())
            .with_confirmed(confirmed);
        let mut result = self.inner.execute(&config).await?;

        // ── Confirmation gate (Claude Code style) ───────────────────
        // When the sandbox requires user confirmation, BLOCK the tool
        // on a oneshot channel. The SSE stream notifies the frontend,
        // and the /api/sandbox/confirm endpoint resolves the oneshot.
        // The LLM never sees the confirmation — it's transparent.
        if result.needs_confirmation {
            let reason = result.confirmation_reason.clone();

            // Create oneshot — we'll wait for the user's response
            let (tx, rx) = tokio::sync::oneshot::channel();

            // Register the pending confirmation so the confirm endpoint can resolve it
            self.confirmations.write().await.insert(self.session_id, PendingConfirmation {
                command: command.to_string(),
                reason: reason.clone(),
                response_tx: tx,
            });

            // Notify the SSE stream so the frontend shows a dialog
            let _ = self.notif_tx.send(ConfirmationNotification {
                session_id: self.session_id,
                command: command.to_string(),
                reason: reason.clone(),
            });

            tracing::info!(
                session_id = %self.session_id,
                command = %command,
                %reason,
                "Waiting for user confirmation..."
            );

            // BLOCK until user clicks Allow or Deny
            let approved = rx.await.unwrap_or(false);

            // Clean up pending confirmation
            self.confirmations.write().await.remove(&self.session_id);

            if !approved {
                tracing::info!(
                    session_id = %self.session_id,
                    command = %command,
                    "User denied execution"
                );
                return Ok(everevo_core::tool::ToolOutput {
                    content: format!("User denied execution: {reason}"),
                    is_error: true,
                });
            }

            tracing::info!(
                session_id = %self.session_id,
                command = %command,
                "User approved — re-executing with confirmed=true"
            );

            // Re-execute with user confirmation
            let config = everevo_core::sandbox::ExecutionConfig::new(command)
                .with_timeout(timeout_secs)
                .with_working_dir(self.work_dir.clone())
                .with_confirmed(true);
            result = self.inner.execute(&config).await?;
        }

        let content = if result.stdout.is_empty() { result.stderr.clone() } else { result.stdout.clone() };
        let is_error = result.exit_code != 0 || result.killed_by_timeout;
        if result.killed_by_timeout {
            return Ok(everevo_core::tool::ToolOutput { content: format!("Timeout after {timeout_secs}s"), is_error: true });
        }
        if result.exit_code == 126 {
            return Ok(everevo_core::tool::ToolOutput { content, is_error: true });
        }
        Ok(everevo_core::tool::ToolOutput { content, is_error })
    }
}

fn db_message_to_llm(m: &MessageRow) -> LlmMessage {
    let role = match m.role.as_str() {
        "user" => LlmRole::User,
        "assistant" => LlmRole::Assistant,
        "system" => LlmRole::System,
        "tool" => LlmRole::Tool,
        _ => LlmRole::User,
    };
    LlmMessage {
        role,
        content: m.content.clone(),
        thinking: None,
        tool_calls: m
            .tool_calls
            .as_ref()
            .and_then(|tc| serde_json::from_str(tc).ok()),
        tool_call_id: m.tool_call_id.clone(),
    }
}

fn resolve_permission(level: &str) -> everevo_sandbox::PermissionLevel {
    match level {
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        _ => everevo_sandbox::PermissionLevel::SemiAuto,
    }
}
