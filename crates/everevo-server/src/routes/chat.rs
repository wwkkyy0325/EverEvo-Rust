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
use everevo_core::context::{default_pipeline, ContextBuildContext};
use everevo_core::llm::{LlmMessage, LlmRole};
use everevo_core::types::ChatRequest;
use everevo_db::models::MessageRow;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::app_state::{AppState, ConfirmationNotification};
use crate::orchestration::{self, ContentBlockStreamer};

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
            let _ = tx.send(Ok(Event::default().event("error").data(&e))).await;
        }
    });

    Sse::new(ReceiverStream::new(rx))
}

async fn handle_chat(
    state: Arc<AppState>,
    req: ChatRequest,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), String> {
    // ── 0. Reconnection mode — replay messages from DB ──────────────
    if req.reconnect {
        return handle_reconnect(&state, req, tx).await;
    }

    // ── 1. Resolve session + load history ────────────────────────────
    let (session_id, history) =
        orchestration::resolve_session(&state, req.session_id, &req.message)
            .await
            .map_err(|e| e.message)?;

    // ── 1.5. Slash command dispatch ──────────────────────────────────
    let effective_message = if let Some((cmd, args)) = state.commands.parse(&req.message) {
        match cmd {
            "help" => {
                let help_text = state.commands.help_text();
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "help"}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                help_text
            }
            "clear" => {
                // Delete all messages in this session and start fresh.
                if let Err(e) = state.db.delete_session_messages(session_id).await {
                    tracing::warn!(%session_id, error = %e, "Failed to clear session messages");
                }
                let _ = tx.send(Ok(Event::default()
                    .event("session_cleared")
                    .json_data(serde_json::json!({"session_id": session_id.to_string()}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                tracing::info!(%session_id, "Session cleared via /clear");
                "Conversation history cleared. Starting fresh.".to_string()
            }
            "compact" => {
                let topic = if args.is_empty() { "recent discussion" } else { args };
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "compact", "topic": topic}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                format!(
                    "Summarize and compact the conversation history, preserving key context. \
                     Focus on: {topic}.\n\n\
                     Generate a structured summary covering decisions, code changes, \
                     and open issues. The compacted summary will replace the full history."
                )
            }
            "memory" => {
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "memory", "query": args}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                if args.is_empty() {
                    "Search my persistent memory for relevant facts, preferences, \
                     and past decisions. List the most relevant findings."
                        .to_string()
                } else {
                    format!(
                        "Search my persistent memory for: {args}\n\n\
                         Find relevant facts, preferences, and past decisions. \
                         Report findings with their sources."
                    )
                }
            }
            "config" => {
                let status = state
                    .startup_report
                    .read()
                    .await
                    .as_ref()
                    .map(|r| {
                        format!(
                            "Port: {}\nChecks: {} pass / {} warn / {} fail\nData: {}\nONNX: loaded\nLLM: configured",
                            r.actual_port, r.pass, r.warn, r.fail,
                            state.config.data_dir.display()
                        )
                    })
                    .unwrap_or_else(|| "Config not yet loaded".to_string());
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "config"}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                format!("## Current Configuration\n\n{status}")
            }
            "plan" => {
                let plan_task = args;
                if plan_task == "cancel" || plan_task == "exit" {
                    state.plan_mode_sessions.write().await.remove(&session_id);
                    let _ = tx.send(Ok(Event::default()
                        .event("plan_mode_exited")
                        .json_data(serde_json::json!({"session_id": session_id.to_string()}))
                        .unwrap_or_else(|_| Event::default().event("error")))).await;
                    tracing::info!(%session_id, "Plan mode cancelled by user");
                    "Plan mode cancelled. Normal operations resumed.".to_string()
                } else {
                    state.plan_mode_sessions.write().await.insert(session_id, "semi_auto".to_string());
                    let _ = tx.send(Ok(Event::default()
                        .event("plan_mode_entered")
                        .json_data(serde_json::json!({"session_id": session_id.to_string(), "task": plan_task}))
                        .unwrap_or_else(|_| Event::default().event("error")))).await;
                    tracing::info!(%session_id, task = plan_task, "Plan mode entered via /plan command");
                    if plan_task.is_empty() {
                        "Plan mode entered via /plan. Explore the codebase, design an approach, \
                         and write a plan. Write tools are blocked until the user approves.".to_string()
                    } else {
                        format!(
                            "Plan mode entered for: {plan_task}\n\n\
                             Explore the codebase, design an approach, and write a plan. \
                             Write tools (shell, write_file, download) are blocked until approval."
                        )
                    }
                }
            }
            "tasks" => {
                let todos = state.todo_store.read().await;
                let items = todos.get(&session_id);
                let summary = if let Some(items) = items {
                    if items.is_empty() {
                        "No active tasks. Use TodoWrite to create a task list.".to_string()
                    } else {
                        let lines: Vec<String> = items
                            .iter()
                            .map(|t| {
                                let icon = match t.status.as_str() {
                                    "completed" => "✅",
                                    "in_progress" => "🔄",
                                    _ => "⏳",
                                };
                                format!("{icon} **{}** — {}", t.content, t.status)
                            })
                            .collect();
                        format!("## Current Tasks\n\n{}\n\nUse TodoWrite to manage.", lines.join("\n"))
                    }
                } else {
                    "No task list found. Use TodoWrite to create one.".to_string()
                };
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "tasks"}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                summary
            }
            "doctor" => {
                let report = state.startup_report.read().await;
                let status = if let Some(ref r) = *report {
                    let checks: Vec<String> = r
                        .items
                        .iter()
                        .map(|c| {
                            let icon = match c.status {
                                crate::startup_check::CheckStatus::Pass => "✅",
                                crate::startup_check::CheckStatus::Warn => "⚠️",
                                _ => "❌",
                            };
                            format!("{icon} **{}**: {} ({}ms)", c.name, c.detail, c.latency_ms)
                        })
                        .collect();
                    format!(
                        "## System Health\n\nPort: {}\nLLM: {} provider(s)\nRAG: active\nMemory: facts loaded\n\n### Checks\n\n{}\n\n**{} pass / {} warn / {} fail**",
                        r.actual_port,
                        state.llm.read().await.len(),
                        checks.join("\n"),
                        r.pass, r.warn, r.fail,
                    )
                } else {
                    "System health report not yet available. Try again after startup completes.".to_string()
                };
                let _ = tx.send(Ok(Event::default()
                    .event("slash_command")
                    .json_data(serde_json::json!({"command": "doctor"}))
                    .unwrap_or_else(|_| Event::default().event("error")))).await;
                status
            }
            _ => req.message.clone(),
        }
    } else {
        req.message.clone()
    };

    // ── 1.5 Start telemetry trace for this session ──────────────────
    let trace = state.telemetry.start_trace(session_id);
    let trace_id = trace.as_ref().map(|t| t.trace_id);

    // ── 3. Build context via pipeline ─────────────────────────────────
    let (shell_name, permission_level, trusted_paths) = {
        let sandboxes = state.sandboxes.read().await;
        sandboxes
            .get(&session_id)
            .map(|sb| {
                (
                    sb.engine().shell_name().to_string(),
                    sb.permission_level().label().to_string(),
                    sb.trusted_paths(),
                )
            })
            .unwrap_or_default()
    };
    // Base tools: shell, download, bootstrap, memory, TodoWrite, EnterPlanMode,
    // ExitPlanMode, Workflow, Skill, Verify, Task, WebFetch, Compact, Team,
    // WorkflowRunner, CodeSearch, CodeMap = 17
    let base_tool_count = 17usize;
    let mcp_tool_count: usize = state
        .mcp_clients
        .read()
        .await
        .values()
        .filter_map(|c| c.try_lock().ok().map(|g| g.tools.len()))
        .sum();
    let tool_count = base_tool_count + mcp_tool_count;
    let workspace_path = state.workspace_dir.read().await.clone();
    let workspace_path_str = workspace_path.as_ref().map(|p| p.display().to_string());

    // Git detection (Claude Code alignment)
    let (git_branch, git_status) = workspace_path
        .as_ref()
        .map(|ws| detect_git(ws))
        .unwrap_or((None, None));

    // CLAUDE.md / AGENTS.md auto-discovery (Claude Code alignment)
    let workspace_context_files = workspace_path
        .as_ref()
        .map(|ws| discover_workspace_context(ws))
        .unwrap_or_default();

    // Build todo summary for the TaskStateStage — lets the agent distinguish
    // pending from completed work and correctly interpret "继续" (continue).
    let todo_summary = {
        let store = state.todo_store.read().await;
        store.get(&session_id).map(|items| {
            if items.is_empty() {
                "(empty)".to_string()
            } else {
                items.iter().map(|item| {
                    let icon = match item.status.as_str() {
                        "completed" => "✅",
                        "in_progress" => "🔄",
                        _ => "⬜",
                    };
                    format!("- {} {} ({})", icon, item.content, item.status)
                }).collect::<Vec<_>>().join("\n")
            }
        })
    };

    let ctx = ContextBuildContext {
        user_message: effective_message.clone(),
        session_id: Some(session_id),
        session_title: None,
        history,
        history_tokens: 0,
        max_context_tokens: state.config.max_context_tokens,
        shell_name: Some(shell_name.clone()),
        permission_level: Some(permission_level.clone()),
        trusted_paths,
        tool_count,
        workspace_path: workspace_path_str,
        platform: Some(std::env::consts::OS.to_string()),
        git_branch,
        git_status,
        workspace_context_files,
        current_date: Some(chrono::Utc::now().format("%Y-%m-%d").to_string()),
        todo_summary: todo_summary.clone(),
        plan_mode: {
            let ps = state.plan_mode_sessions.read().await;
            ps.contains_key(&session_id)
        },
        escalation_level: None,
        fixation_detail: None,
    };
    let persona_profile_path = state
        .config
        .data_dir
        .join("memory")
        .join("persona")
        .join("profile.json");
    let memory_stage = {
        let mut stage = everevo_agent::MemoryStage::new(state.fact_manager.clone())
            .with_knowledge_graph(state.knowledge_graph.clone());
        if let Some(ref rag) = state.rag_pipeline {
            stage = stage.with_rag(Arc::clone(rag));
        }
        if let Some(tid) = trace_id {
            stage.with_telemetry(state.telemetry.clone(), tid)
        } else {
            stage
        }
    };
    let domain_root = state.config.data_dir.join("domain");
    let domain_stage = everevo_agent::DomainKnowledgeStage::new(&domain_root).with_max_docs(3);

    // Parent work dir for sub-agent path inheritance.
    let parent_work_dir = state
        .sandboxes
        .read()
        .await
        .get(&session_id)
        .map(|sb| sb.work_dir().clone());

    // Build sub-agent context BEFORE the pipeline consumes domain_stage.
    let shell = shell_name.clone();
    let mut sub_ctx = everevo_agent::subagent_context::assemble_subagent_context(
        &effective_message,
        None,
        Some(&domain_stage),
        parent_work_dir,
        None,
        &shell,
        &["shell".into(), "memory".into()],
        todo_summary.clone(),
    )
    .await;
    // Inherit parent session's permission level for sub-agents.
    sub_ctx.permission_level = Some(permission_level.clone());

    // Inject T1 memory context for sub-agents (≤400 chars)
    if let Ok(t1) = state.fact_manager.load_tier1() {
        if !t1.is_empty() {
            let lines: Vec<String> = t1.iter().take(5).map(|f| {
                format!("- {} — {}", f.name, f.description)
            }).collect();
            sub_ctx.memory_context = Some(lines.join("\n"));
        }
    }
    // Inject KG entity count
    if let Ok(kg) = state.knowledge_graph.read() {
        let ec = kg.entity_count();
        if ec > 0 {
            sub_ctx.kg_context = Some(format!("{ec} entities available. Use `memory` → `kg_search` to explore."));
        }
    }

    let pipeline = default_pipeline()
        .with_stage(everevo_agent::PersonaStage::new(persona_profile_path))
        .with_stage(everevo_agent::BestPracticesStage)
        .with_stage(everevo_agent::SkillStage::new(state.skill_registry.clone()))
        .with_stage(memory_stage)
        .with_stage(domain_stage);

    // Determine turn number (user+assistant pairs → turns)
    let turn_number = ctx.history.len() / 2 + 1;
    let (messages, snapshot) =
        pipeline.assemble_with_snapshot(&ctx, session_id, turn_number);

    // Store snapshot for observability dashboard (fire-and-forget)
    let snapshots_state = Arc::clone(&state);
    tokio::spawn(async move {
        snapshots_state.record_context_snapshot(snapshot).await;
    });

    // ── 4. Persist user message ──────────────────────────────────────
    let user_msg = MessageRow::new(session_id, "user", req.message.clone(), None, None, None);
    state
        .db
        .add_message(&user_msg)
        .await
        .map_err(|e| format!("Failed to save user message: {e}"))?;

    // Feed raw message into the dreaming engine's buffer
    state
        .dreaming_engine
        .push_message("user", &req.message, &user_msg.id.to_string(), &session_id.to_string());

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

    // ── 6. Build tool registry ────────────────────────────────────────
    let assembled = orchestration::build_registry(
        &state,
        session_id,
        &client,
        &notif_tx,
        &permission_level,
        &sub_ctx,
    )
    .await;
    let tools = assembled.tools;
    let pending_subagents = assembled.pending;
    let subagent_rx = assembled.subagent_rx;
    let results_backlog = assembled.results_backlog;
    let compact_focus = assembled.compact_focus;

    // ── Register cancellable session + disconnect watcher ────────────
    let session_cancel = tokio_util::sync::CancellationToken::new();
    state
        .session_actors
        .write()
        .await
        .insert(session_id, session_cancel.clone());

    let tx_disconnect = tx.clone();
    let cancel_on_disconnect = session_cancel.clone();
    tokio::spawn(async move {
        tx_disconnect.closed().await;
        cancel_on_disconnect.cancel();
        tracing::info!("SSE client disconnected — session cancelled");
    });

    // ── 7. Run Agent Loop — content-block SSE streaming ───────────────
    //
    // Events follow Anthropic's content-block model:
    //   message_start → content_block_start/delta/stop (repeated)
    //   → message_delta → message_stop
    //
    // Each thinking / tool_use / text segment is a separate content block
    // with an incrementing index.  This lets the frontend render blocks
    // in order without any interleaving hacks.
    // Clone refs before moving into AgentLoop (needed for auto-continue)
    let pending_for_autocontinue = Arc::clone(&pending_subagents);
    let mut messages_for_autocontinue = messages.clone();
    let client_for_autocontinue = Arc::clone(&client);
    let tools_for_autocontinue = Arc::clone(&tools);
    let proactivity = Arc::new(std::sync::Mutex::new(
        everevo_agent::ProactivityState::new(),
    ));
    let agent = everevo_agent::AgentLoop::new()
        .with_subagent_channel(subagent_rx)
        .with_pending_subagents(pending_subagents)
        .with_cancel_token(session_cancel.clone())
        .with_compact_focus(compact_focus.clone())
        .with_proactivity(Arc::clone(&proactivity));
    let mut agent_rx = agent.run(client, tools, messages, None).await;

    let assistant_id = Uuid::new_v4();
    let mut s = ContentBlockStreamer::new(session_id);

    // ── message_start ──
    let _ = tx
        .send(orchestration::stream::message_start(
            &assistant_id.to_string(),
        ))
        .await;

    let mut agent_yielded_for_subagents = false;
    loop {
        tokio::select! {
            // ── Agent events (primary channel) ──────────────────────
            event = agent_rx.recv() => {
                match event {
                    Some(ev) => {
                        let is_turn = matches!(ev, everevo_agent::AgentEvent::TurnComplete);
                        let is_waiting = matches!(ev, everevo_agent::AgentEvent::WaitingForSubAgents { .. });
                        if is_waiting {
                            tracing::info!("Main loop yielded — waiting for sub-agents");
                            agent_yielded_for_subagents = true;
                        }
                        match s.handle_event(ev, tx).await {
                            crate::orchestration::StreamerAction::Continue => {
                                if is_turn {
                                    // Per-tool persistence
                                    for (_tc_id, tc_json, thinking) in s.pending_stubs.drain(..) {
                                        let _ = state.db.add_message(&MessageRow::new(
                                            session_id, "assistant", "",
                                            Some(serde_json::to_string(&[tc_json]).unwrap_or_default()),
                                            None, thinking,
                                        )).await;
                                    }
                                    for (tc_id, tc_content, _tc_err) in s.pending_results.drain(..) {
                                        let _ = state.db.add_message(&MessageRow::new(
                                            session_id, "tool", tc_content, None, Some(tc_id), None,
                                        )).await;
                                    }
                                    state.scheduler.increment_turn();
                                }
                            }
                            crate::orchestration::StreamerAction::Done => break,
                            crate::orchestration::StreamerAction::Error { .. } => break,
                        }
                    }
                    None => break,
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

    // ── 7.5 Auto-continue: sub-agent results arrive → restart agent loop ──
    // Guard against infinite restarts: max 5 auto-continue cycles, and
    // break if pending_subagents hasn't decreased between cycles.
    if agent_yielded_for_subagents {
        let mut drained = 0usize;
        let mut auto_cycles = 0u32;
        const MAX_AUTO_CYCLES: u32 = 5;
        let mut last_pending = pending_for_autocontinue.load(std::sync::atomic::Ordering::SeqCst);

        loop {
            auto_cycles += 1;
            if auto_cycles > MAX_AUTO_CYCLES {
                tracing::warn!(
                    cycles = auto_cycles,
                    "Auto-continue limit reached — forcing final synthesis"
                );
                break;
            }

            let pending = pending_for_autocontinue.load(std::sync::atomic::Ordering::SeqCst);

            // ── Extract new results (drop lock before any await) ──
            let new_results: Vec<(String, String, String)> = {
                let backlog = results_backlog.lock().unwrap_or_else(|e| e.into_inner());
                if drained < backlog.len() {
                    backlog[drained..].to_vec()
                } else {
                    Vec::new()
                }
            };

            if pending == 0 && new_results.is_empty() {
                tracing::info!("All sub-agents completed — final synthesis");
                // Inject verification nudge so the LLM can call Verify tool
                messages_for_autocontinue.push(everevo_core::llm::LlmMessage::user(
                    "All sub-agent tasks have completed. Review the results above. \
                     If any task output needs verification, use the Verify tool \
                     to check correctness before providing your final answer.",
                ));
                break;
            }

            // If pending count hasn't changed and no new results, the sub-agents
            // might be stuck — force final synthesis instead of looping forever.
            if new_results.is_empty() {
                if pending >= last_pending {
                    tracing::warn!(
                        pending,
                        cycles = auto_cycles,
                        "Sub-agents stalled — forcing final synthesis"
                    );
                    break;
                }
                last_pending = pending;
                // No new results — sleep and retry
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            last_pending = pending;

            drained = {
                let backlog = results_backlog.lock().unwrap_or_else(|e| e.into_inner());
                backlog.len()
            };

            // ── Inject results and send SSE events ──
            for (task_id, desc, result) in &new_results {
                let short: String = result.chars().take(2000).collect();
                let _ = tx
                    .send(Ok(Event::default()
                        .event("subagent_result")
                        .data(serde_json::json!({"id": task_id, "description": desc, "result": short}).to_string())))
                    .await;
                messages_for_autocontinue.push(everevo_core::llm::LlmMessage::user(format!(
                    "[SubAgent Result]\n{result}"
                )));
            }

            // ── Restart AgentLoop with updated messages ──
            let agent2 = everevo_agent::AgentLoop::new()
                .with_pending_subagents(Arc::clone(&pending_for_autocontinue))
                .with_cancel_token(session_cancel.clone())
                .with_compact_focus(compact_focus.clone());
            let resumed_msgs = messages_for_autocontinue.clone();
            let mut agent_rx2 = agent2
                .run(
                    Arc::clone(&client_for_autocontinue),
                    Arc::clone(&tools_for_autocontinue),
                    resumed_msgs,
                    None,
                )
                .await;

            // ── Stream events from the resumed run (content-block format) ──
            let mut ac_streamer = ContentBlockStreamer::new(session_id);
            ac_streamer.block_index = s.block_index;
            loop {
                tokio::select! {
                    event = agent_rx2.recv() => {
                        match event {
                            Some(ev) => {
                                let is_terminal = matches!(ev,
                                    everevo_agent::AgentEvent::Done { .. } |
                                    everevo_agent::AgentEvent::Error { .. }
                                );
                                match ac_streamer.handle_event(ev, tx).await {
                                    crate::orchestration::StreamerAction::Continue => {
                                        if is_terminal { break; }
                                    }
                                    crate::orchestration::StreamerAction::Done => break,
                                    crate::orchestration::StreamerAction::Error { .. } => break,
                                }
                            }
                            None => break,
                        }
                    }
                    Some(notif) = notif_rx.recv() => {
                        let payload = serde_json::json!({
                            "session_id": notif.session_id.to_string(),
                            "command": notif.command,
                            "reason": notif.reason,
                        });
                        let _ = tx.send(Ok(Event::default()
                            .event("confirmation_required")
                            .data(payload.to_string())
                        )).await;
                    }
                }
            }
            // Sync streamer state back
            s.block_index = ac_streamer.block_index;
            s.thinking_open = ac_streamer.thinking_open;
            s.text_block_idx = ac_streamer.text_block_idx;
            s.full_response = ac_streamer.full_response;

            // Check if all sub-agents are done
            if pending_for_autocontinue.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                break;
            }
        }
    }

    // ── 8-11. Persist + close blocks + cleanup ───────────────────────
    let _ = orchestration::finalize_response(
        tx,
        &state,
        session_id,
        assistant_id,
        &s.full_response,
        &s.cur_thinking,
        &s.persisted_blocks,
        s.thinking_open,
        s.text_block_idx,
        s.block_index,
    )
    .await;

    // ── Post-turn memory extraction (Mem0 pattern: async LLM extraction) ──
    if !s.full_response.is_empty() {
        let llm = state.llm.read().await;
        if let Some(primary) = llm.values().find_map(|v| v.clone()) {
            let fm = state.fact_manager.clone();
            let user_msg = req.message.clone();
            let assistant_msg = s.full_response.clone();
            tokio::spawn(async move {
                everevo_agent::memory::extractor::extract_from_turn(
                    &primary,
                    &fm,
                    &user_msg,
                    &assistant_msg,
                )
                .await;
            });
        }
    }

    Ok(())
}

// ── Reconnection handler ─────────────────────────────────────────────────

/// Replay all messages from DB as SSE events — for reconnecting to
/// background/daemon sessions. Also notifies if the session is still running.
async fn handle_reconnect(
    state: &Arc<AppState>,
    req: ChatRequest,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), String> {
    let session_id = req.session_id.ok_or("session_id required for reconnect")?;

    // Verify session exists
    let session = state
        .db
        .get_session(session_id)
        .await
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| "Session not found".to_string())?;

    // Parse metadata
    let meta: everevo_core::types::SessionMeta =
        serde_json::from_str(&session.metadata).unwrap_or_default();

    // Send session info event
    let _ = tx
        .send(Ok(Event::default()
            .event("session_info")
            .data(serde_json::json!({
                "session_id": session_id,
                "mode": meta.mode.as_str(),
                "state": meta.state.as_str(),
            })
            .to_string())))
        .await;

    // Load all messages
    let messages = state
        .db
        .get_messages(session_id, None)
        .await
        .map_err(|e| format!("Load messages: {e}"))?;

    // Replay messages as SSE events
    for msg in &messages {
        let event_type = match msg.role.as_str() {
            "user" => "user_message",
            "assistant" => "assistant_message",
            "tool" => "tool_message",
            _ => "message",
        };
        let _ = tx
            .send(Ok(Event::default()
                .event(event_type)
                .data(serde_json::json!({
                    "id": msg.id,
                    "role": msg.role,
                    "content": msg.content,
                    "created_at": msg.created_at,
                })
                .to_string())))
            .await;
    }

    // Check if session is still running (has a bg worker)
    let is_running = state.bg_sessions.read().await.contains_key(&session_id);

    if is_running {
        // Session is still active — hold connection open and poll for new messages
        let mut last_count = messages.len();
        // Poll every 500ms for new messages, up to 5 minutes
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Check if still running
            if !state.bg_sessions.read().await.contains_key(&session_id) {
                break; // bg worker finished
            }

            // Check for new messages
            let current = state
                .db
                .get_messages(session_id, None)
                .await
                .map_err(|e| format!("Poll messages: {e}"))?;

            // Send any new messages
            for msg in &current[last_count..] {
                let _ = tx
                    .send(Ok(Event::default()
                        .event("new_message")
                        .data(serde_json::json!({
                            "id": msg.id,
                            "role": msg.role,
                            "content": msg.content,
                            "created_at": msg.created_at,
                        })
                        .to_string())))
                    .await;
            }
            last_count = current.len();
        }
    }

    // Done
    let _ = tx
        .send(Ok(Event::default()
            .event("reconnect_done")
            .data(serde_json::json!({
                "session_id": session_id,
                "message_count": messages.len(),
            })
            .to_string())))
        .await;

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────

pub(crate) fn truncate_for_title(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    if first_line.chars().count() > 60 {
        first_line.chars().take(57).chain("...".chars()).collect()
    } else {
        first_line.to_string()
    }
}

pub(crate) fn db_message_to_llm(m: &MessageRow) -> LlmMessage {
    let role = match m.role.as_str() {
        "user" => LlmRole::User,
        "assistant" => LlmRole::Assistant,
        "system" => LlmRole::System,
        "tool" => LlmRole::Tool,
        _ => LlmRole::User,
    };
    // Only restore thinking for tool-call turns (DeepSeek Rule B).
    // Final answers without tool calls must drop thinking (Rule A).
    let has_tools = m
        .tool_calls
        .as_ref()
        .and_then(|tc| serde_json::from_str::<Vec<serde_json::Value>>(tc).ok())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);
    let thinking = if has_tools && !m.thinking.is_empty() {
        Some(m.thinking.clone())
    } else {
        None
    };
    LlmMessage {
        role,
        content: m.content.clone(),
        thinking,
        tool_calls: m
            .tool_calls
            .as_ref()
            .and_then(|tc| serde_json::from_str(tc).ok()),
        tool_call_id: m.tool_call_id.clone(),
    }
}

pub(crate) fn resolve_permission(level: &str) -> everevo_sandbox::PermissionLevel {
    match level {
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        _ => everevo_sandbox::PermissionLevel::SemiAuto,
    }
}

// ── Git Detection ──────────────────────────────────────────────────────────

/// Detect git repository info for the workspace (Claude Code alignment).
/// Uses std::process to run git CLI — this runs at context-build time
/// (NOT inside the sandbox tool), so sandbox restrictions don't apply.
#[allow(clippy::disallowed_methods)]
fn detect_git(workspace: &std::path::Path) -> (Option<String>, Option<String>) {
    let git_dir = workspace.join(".git");
    if !git_dir.exists() {
        return (None, None);
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let modified = s.lines().filter(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty() && !trimmed.starts_with("??")
            }).count();
            let untracked = s.lines().filter(|l| l.trim().starts_with("??")).count();
            let mut parts = Vec::new();
            if modified > 0 { parts.push(format!("{modified} modified")); }
            if untracked > 0 { parts.push(format!("{untracked} untracked")); }
            if parts.is_empty() { "clean".to_string() } else { parts.join(", ") }
        });
    (branch, status)
}

// ── Workspace Context Discovery ─────────────────────────────────────────────

/// Walk up from workspace root discovering CLAUDE.md / AGENTS.md files
/// (Claude Code alignment — hierarchical context chain).
fn discover_workspace_context(
    workspace: &std::path::Path,
) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut current = Some(workspace.to_path_buf());
    while let Some(dir) = current {
        for name in &["CLAUDE.md", "AGENTS.md", ".everevo.md"] {
            let path = dir.join(name);
            if path.exists() && path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        files.push((path.display().to_string(), trimmed));
                    }
                }
            }
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    // Reverse so root-level files come first, workspace-level last (root-to-leaf)
    files.reverse();
    files
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_for_title ─────────────────────────────────────────

    #[test]
    fn test_truncate_short_text() {
        assert_eq!(truncate_for_title("Hello"), "Hello");
    }

    #[test]
    fn test_truncate_trim_and_first_line() {
        assert_eq!(truncate_for_title("  Hi\nSecond line\nThird  "), "Hi");
    }

    #[test]
    fn test_truncate_long_text() {
        let long = "a".repeat(100);
        let result = truncate_for_title(&long);
        assert_eq!(result.len(), 60); // 57 chars + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_exactly_60() {
        let exact = "a".repeat(60);
        assert_eq!(truncate_for_title(&exact), exact); // no truncation
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_for_title(""), "");
    }

    // ── resolve_permission ─────────────────────────────────────────

    #[test]
    fn test_resolve_permission_known_levels() {
        assert_eq!(
            resolve_permission("fully_auto"),
            everevo_sandbox::PermissionLevel::FullyAuto
        );
        assert_eq!(
            resolve_permission("fully_manual"),
            everevo_sandbox::PermissionLevel::FullyManual
        );
        assert_eq!(
            resolve_permission("read_only"),
            everevo_sandbox::PermissionLevel::ReadOnly
        );
    }

    #[test]
    fn test_resolve_permission_default_semiauto() {
        // Unknown/invalid levels default to SemiAuto
        assert_eq!(
            resolve_permission("unknown"),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
        assert_eq!(
            resolve_permission(""),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
    }

    #[test]
    fn test_resolve_permission_case_sensitive() {
        assert_eq!(
            resolve_permission("Fully_Auto"),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
    }
}
