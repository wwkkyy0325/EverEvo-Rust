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
use everevo_core::context::{ContextBudget, ContextBuildContext};
use everevo_core::types::ChatRequest;
use everevo_db::models::MessageRow;
use futures::FutureExt;
use std::convert::Infallible;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::orchestration::{self, ContentBlockStreamer};

use super::helpers::{detect_git, discover_workspace_context, truncate_for_title};
use super::post_turn::spawn_post_turn_tasks;
use super::reconnect::handle_reconnect;
use super::slash_commands::{
    handle_character_command, handle_plan_command, handle_workspace_command,
};
use super::wiring::apply_session_agent_wiring;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/chat", post(handler))
}

async fn handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let session_id = req.session_id;
        let state_for_err = Arc::clone(&state);
        match AssertUnwindSafe(handle_chat(state, req, &tx))
            .catch_unwind()
            .await
        {
            Ok(Ok(())) => {} // normal completion
            Ok(Err(e)) => {
                // Session lifecycle: agent run terminated with an error.
                if let Some(sid) = session_id {
                    orchestration::set_session_state(
                        &state_for_err.db,
                        sid,
                        everevo_core::types::SessionState::Failed,
                    )
                    .await;
                }
                let _ = tx.send(Ok(Event::default().event("error").data(e))).await;
            }
            Err(panic) => {
                let msg = panic_message(&panic);
                tracing::error!(%msg, "Chat handler panicked — recovered by catch_unwind");
                if let Some(sid) = session_id {
                    orchestration::set_session_state(
                        &state_for_err.db,
                        sid,
                        everevo_core::types::SessionState::Failed,
                    )
                    .await;
                }
                let _ = tx
                    .send(Ok(Event::default()
                        .event("error")
                        .data("Internal server error")))
                    .await;
            }
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

    // Durable rolling summary (spec D3) — injected before the history window.
    // Best-effort: a DB error just means no summary this turn.
    let rolling_summary = state
        .db
        .get_session_context(session_id)
        .await
        .ok()
        .and_then(|(s, _wm)| s);

    // ── 1.5. Slash command dispatch ──────────────────────────────────
    let effective_message = if let Some((cmd, args)) = state.commands.parse(&req.message) {
        match cmd {
            "help" => {
                let help_text = state.commands.help_text();
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "help"}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
                help_text
            }
            "clear" => {
                // Delete all messages in this session and start fresh.
                if let Err(e) = state.db.delete_session_messages(session_id).await {
                    tracing::warn!(%session_id, error = %e, "Failed to clear session messages");
                }
                let _ = tx
                    .send(Ok(Event::default()
                        .event("session_cleared")
                        .json_data(serde_json::json!({"session_id": session_id.to_string()}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
                tracing::info!(%session_id, "Session cleared via /clear");
                "Conversation history cleared. Starting fresh.".to_string()
            }
            "compact" => {
                let topic = if args.is_empty() {
                    "recent discussion"
                } else {
                    args
                };
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "compact", "topic": topic}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
                format!(
                    "Summarize and compact the conversation history, preserving key context. \
                     Focus on: {topic}.\n\n\
                     Generate a structured summary covering decisions, code changes, \
                     and open issues. The compacted summary will replace the full history."
                )
            }
            "memory" => {
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "memory", "query": args}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
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
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "config"}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
                format!("## Current Configuration\n\n{status}")
            }
            "character" => handle_character_command(&state, tx, args).await,
            "plan" => handle_plan_command(&state, session_id, tx, args).await,
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
                        format!(
                            "## Current Tasks\n\n{}\n\nUse TodoWrite to manage.",
                            lines.join("\n")
                        )
                    }
                } else {
                    "No task list found. Use TodoWrite to create one.".to_string()
                };
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "tasks"}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
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
                    "System health report not yet available. Try again after startup completes."
                        .to_string()
                };
                let _ = tx
                    .send(Ok(Event::default()
                        .event("slash_command")
                        .json_data(serde_json::json!({"command": "doctor"}))
                        .unwrap_or_else(|_| Event::default().event("error"))))
                    .await;
                status
            }
            "workspace" => handle_workspace_command(&state, session_id, tx, args).await,
            _ => req.message.clone(),
        }
    } else {
        req.message.clone()
    };

    // ── 1.5 Start telemetry trace for this session ──────────────────
    let trace = state.telemetry_pipeline.start_trace(session_id);
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
    // Tool count: MCP tools (known at build time) + base estimate of
    // bootstrap (6) + in-process (~17) = 23. The exact total is determined
    // during assemble() — the system prompt shows this estimate, while the
    // actual tool schemas sent to the LLM reflect the real count.
    let mcp_tool_count: usize = state
        .mcp_clients
        .read()
        .await
        .values()
        .filter_map(|c| c.try_lock().ok().map(|g| g.tools.len()))
        .sum();
    let base_tool_count: usize = 23; // 6 bootstrap + ~17 in-process (always registered)
    let tool_count = base_tool_count + mcp_tool_count;
    // Use per-session sandbox work_dir (may be workspace or sandbox default)
    let workspace_path = {
        let sandboxes = state.sandboxes.read().await;
        sandboxes.get(&session_id).map(|sb| sb.work_dir().clone())
    };
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
        let fmt_items = |items: &[everevo_agent::tools::builtins::TodoItem]| {
            items
                .iter()
                .map(|item| {
                    let icon = match item.status.as_str() {
                        "completed" => "✅",
                        "in_progress" => "🔄",
                        _ => "⬜",
                    };
                    format!("- {} {} ({})", icon, item.content, item.status)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let global = store
            .get(&everevo_agent::tools::builtins::GLOBAL_TASK_KEY)
            .filter(|g| !g.is_empty());
        let session = store.get(&session_id).filter(|s| !s.is_empty());
        match (global, session) {
            (Some(g), Some(s)) => format!(
                "## Global (cross-conversation):\n{}\n\n## This session:\n{}",
                fmt_items(g),
                fmt_items(s)
            ),
            (Some(g), None) => format!("## Global (cross-conversation):\n{}", fmt_items(g)),
            (None, Some(s)) => fmt_items(s),
            (None, None) => "(empty)".to_string(),
        }
    };

    // Per-model context budget: resolved from the main provider's
    // `context_window` (128k floor when unset) — the main session finally tracks
    // the model actually in use instead of the 100k/80k hardcoded values.
    let context_budget = ContextBudget::resolve(
        state
            .main_llm
            .read()
            .await
            .as_ref()
            .and_then(|r| r.context_window),
    );
    // Capture before the budget is moved into the context (used again for the
    // AgentRun ceiling below).
    let context_window_tokens = context_budget.window;
    let ctx = ContextBuildContext {
        user_message: effective_message.clone(),
        session_id: Some(session_id),
        session_title: None,
        history,
        history_tokens: 0,
        max_context_tokens: context_window_tokens,
        budget: context_budget,
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
        todo_summary: Some(todo_summary.clone()),
        plan_mode: {
            let ps = state.plan_mode_sessions.read().await;
            ps.contains_key(&session_id)
        },
        runtime_summary: {
            let report = state.startup_report.read().await;
            report.as_ref().and_then(|r| {
                r.items
                    .iter()
                    .find(|c| c.name == "Runtime smoke test")
                    .map(|c| c.detail.clone())
            })
        },
        sandbox_root: Some(
            state
                .config
                .data_dir
                .join("sandbox")
                .join(session_id.to_string())
                .display()
                .to_string(),
        ),
        startup_verified: {
            let report = state.startup_report.read().await;
            report.as_ref().map(|r| r.fail == 0).unwrap_or(false)
        },
        hook_feedback: None,
        summary: rolling_summary,
    };
    let persona_profile_path = state
        .config
        .data_dir
        .join("memory")
        .join("persona")
        .join("profile.json");
    let agent_char_path = state
        .config
        .data_dir
        .join("memory")
        .join("agent")
        .join("character.json");
    let memory_stage = state.build_memory_stage(session_id, trace_id);
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
    // Gather skill list for sub-agent context
    let skill_list = {
        let skills = state.skill_registry.list_metadata();
        if skills.is_empty() {
            None
        } else {
            Some(
                skills
                    .iter()
                    .map(|(name, desc)| format!("- **{name}**: {desc}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    };
    // LLM-free meta-orchestrator policy layer — strictly opt-in benchmark
    // scaffolding (EVEREVO_BENCHMARK set AND EVEREVO_META_ORCHESTRATOR != "0").
    // None → the loop is byte-equivalent to today. Constructed here so the
    // shared state can also seed the sub-agent phase directive below and the
    // auto-continue restart wires the same state.
    let orchestrator = if crate::app_state::meta_orchestrator_effective() {
        Some(std::sync::Arc::new(std::sync::Mutex::new(
            everevo_agent::loop_::MetaOrchestratorState::new(),
        )))
    } else {
        None
    };
    let mut sub_ctx = everevo_agent::subagent_context::assemble_subagent_context(
        &effective_message,
        None,
        Some(&domain_stage),
        parent_work_dir,
        None,
        &shell,
        &["shell".into(), "memory".into()],
        Some(todo_summary.clone()),
        skill_list,
        Some(context_window_tokens),
    )
    .await;
    // Inherit parent session's permission level for sub-agents.
    sub_ctx.permission_level = Some(permission_level.clone());
    // Seed the sub-agent with the parent's orchestrator phase context (None
    // when the orchestrator is off → the sub-agent system prompt omits the
    // section entirely).
    sub_ctx.orchestrator_directive = orchestrator.as_ref().map(|orch| {
        let phase = orch.lock().unwrap_or_else(|e| e.into_inner()).phase;
        everevo_agent::loop_::subagent_phase_directive(phase)
    });

    // Inject T1 memory context for sub-agents (≤400 chars).
    // Session-filtered: sub-agents of this session must not see other sessions'
    // working memory (分层记忆 strict isolation).
    if let Ok(t1) = state.fact_manager.load_tier1() {
        let t1_visible: Vec<_> = t1
            .into_iter()
            .filter(|f| everevo_agent::memory::facts::fact_visible_to(f, Some(&session_id)))
            .collect();
        if !t1_visible.is_empty() {
            let lines: Vec<String> = t1_visible
                .iter()
                .take(5)
                .map(|f| format!("- {} — {}", f.name, f.description))
                .collect();
            sub_ctx.memory_context = Some(lines.join("\n"));
        }
    }
    // Inject KG entity count
    if let Ok(kg) = state.knowledge_graph.read() {
        let ec = kg.entity_count();
        if ec > 0 {
            sub_ctx.kg_context = Some(format!(
                "{ec} entities available. Use `memory` → `kg_search` to explore."
            ));
        }
    }

    // NOTE: sub-agents are task-focused workers (researchers, reviewers, file
    // operators) whose output returns to the main agent — they do NOT inherit
    // the agent character/voice. Per Claude Code practice + arXiv 2311.10054,
    // persona is a token-cost luxury invisible in sub-agent output. The main
    // agent alone carries the voice; sub-agents keep only the user-persona
    // (language/format) via SubAgentContext.persona below.

    let pipeline = everevo_agent::stages::build_full_pipeline(
        agent_char_path,
        persona_profile_path,
        state.skill_registry.clone(),
        memory_stage,
        domain_stage,
    );

    // Determine turn number (user+assistant pairs → turns)
    let turn_number = ctx.history.len() / 2 + 1;
    let (messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, session_id, turn_number);

    // Store snapshot for observability dashboard (fire-and-forget)
    let snapshots_state = Arc::clone(&state);
    tokio::spawn(async move {
        snapshots_state.record_context_snapshot(snapshot).await;
    });

    // ── 4. Persist user message (single write path: DB + dreaming) ──
    // P2 write convergence — the fan-out lives in SessionContent::persist_user.
    // DB failure is NON-FATAL: a write error must not kill the chat session
    // (error-transition-table "Session / DB" row; audit MEDIUM, 2026-08-13 —
    // this was previously propagated with `?`, contradicting the table's
    // best-effort claim and failing the turn on a storage hiccup).
    if let Err(e) = crate::session_content::SessionContent::new(&state, session_id)
        .persist_user(&req.message)
        .await
    {
        tracing::warn!(%session_id, error = %e, "Failed to persist user message (non-fatal)");
    }

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
    let client_for_tokens = Arc::clone(&client); // Clone before agent.run() moves it

    // ── 6. Build per-session data flow + tool registry ──────────────
    let (mut coord, mut receivers) = crate::orchestration::SessionCoordinator::new(session_id);

    state
        .session_actors
        .write()
        .await
        .insert(session_id, coord.cancel.clone());

    // Session lifecycle: agent run started.
    crate::orchestration::set_session_state(
        &state.db,
        session_id,
        everevo_core::types::SessionState::Running,
    )
    .await;

    let tx_disconnect = tx.clone();
    let cancel_on_disconnect = coord.cancel.clone();
    tokio::spawn(async move {
        tx_disconnect.closed().await;
        cancel_on_disconnect.cancel();
        tracing::info!("SSE client disconnected — session cancelled");
    });

    let assembled = orchestration::build_registry(
        &state,
        session_id,
        &client,
        &mut coord,
        &permission_level,
        &sub_ctx,
    )
    .await;
    let tools = assembled.tools;
    let subagent_rx = assembled.subagent_rx;

    // ── 7. Run Agent Loop — content-block SSE streaming ───────────────
    //
    // Events follow Anthropic's content-block model:
    //   message_start → content_block_start/delta/stop (repeated)
    //   → message_delta → message_stop
    //
    // Each thinking / tool_use / text segment is a separate content block
    // with an incrementing index.  This lets the frontend render blocks
    // in order without any interleaving hacks.
    // Clone refs before moving into AgentRun (needed for auto-continue)
    let pending_for_autocontinue = Arc::clone(&coord.pending);
    let mut messages_for_autocontinue = messages.clone();
    let client_for_autocontinue = Arc::clone(&client) as Arc<dyn everevo_core::LlmProvider>;
    let tools_for_autocontinue = Arc::clone(&tools);
    let proactivity = Arc::new(std::sync::Mutex::new(everevo_agent::ProactivityState::new()));

    // Compaction model routing (decision 1): the configured compact model
    // (`compactModelId`) when present, else the main model — "有哪个用哪个".
    let (compact_arc, compact_window) = {
        let c = state.compact_llm.read().await;
        match c.as_ref() {
            Some(r) => (
                Some(Arc::clone(&r.client) as Arc<dyn everevo_core::llm::LlmProvider>),
                r.context_window,
            ),
            None => (
                Some(Arc::clone(&client) as Arc<dyn everevo_core::llm::LlmProvider>),
                None,
            ),
        }
    };
    // Meta-agent self-diagnosis is switchable (routing `metaAgentEnabled`,
    // `EVEREVO_META_AGENT` env, benchmark default OFF). Disabled → the loop
    // skips trigger + background `meta_diagnose` entirely. Computed once so the
    // autocontinue restart below wires the same state.
    let meta_agent = if crate::app_state::meta_agent_effective(&state).await {
        let meta_llm = {
            let c = state.compact_llm.read().await;
            c.as_ref()
                .map(|r| Arc::clone(&r.client))
                .unwrap_or_else(|| Arc::clone(&client))
        };
        Some(Arc::new(std::sync::Mutex::new(
            everevo_agent::memory::MetaAgentState::new(
                Some(meta_llm),
                Some(state.fact_manager.clone()),
            ),
        )))
    } else {
        None
    };
    let mut agent = everevo_agent::AgentRun::main_session(
        subagent_rx,
        coord.pending.clone(),
        coord.cancel.clone(),
        coord.compact_focus.clone(),
        Arc::clone(&proactivity),
    );
    // Shared session wiring (proactivity/context-budget/hook-feedback/meta-agent/
    // telemetry/benchmark-turn-cap) — identical for the auto-continue restart.
    agent = apply_session_agent_wiring(
        agent,
        &proactivity,
        &meta_agent,
        &orchestrator,
        &assembled.hook_feedback,
        context_window_tokens,
        trace_id,
        state.telemetry_pipeline.clone(),
    );
    agent = agent.with_compact_llm(compact_arc.clone());
    // TodoWrite awareness: pass the current task list so the driver's evidence
    // gate can reference completed-but-unverified items.
    agent = agent.with_todo_summary(Some(todo_summary.clone()));
    // Layer-1 background rolling-summary maintenance (spec D3): runs at soft
    // threshold turn boundaries without blocking the main loop.
    agent = agent.with_background_maintenance(compact_arc.as_ref().map(|llm| {
        Arc::new(everevo_agent::context::BackgroundMaintenance {
            db: state.db.clone(),
            session_id,
            llm: Arc::clone(llm),
            ctx_window: compact_window,
            in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }));
    // Phase 8 (spec deliverable 6): page large tool outputs to
    // data/sessions/<id>/tool_cache/ so the context keeps only a 2KB preview.
    // The full text stays reachable via the `tool_cache_read` tool.
    agent = agent.with_tool_cache_dir(Some(
        state
            .config
            .data_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("tool_cache"),
    ));
    let mut agent_rx = agent
        .run(
            client as Arc<dyn everevo_core::LlmProvider>,
            tools,
            messages,
            None,
        )
        .await;

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
            Some(notif) = receivers.confirm_rx.recv() => {
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
            // ── Ask-user notifications (ask_user tool → frontend) ──
            Some(ask) = receivers.ask_user_rx.recv() => {
                tracing::info!(
                    session_id = %ask.session_id,
                    question = %ask.question,
                    "Sending awaiting_user to frontend"
                );
                // Session lifecycle: agent blocked waiting for the user.
                crate::orchestration::set_session_state(
                    &state.db,
                    ask.session_id,
                    everevo_core::types::SessionState::WaitingUser,
                )
                .await;
                let payload = serde_json::json!({
                    "session_id": ask.session_id.to_string(),
                    "question": ask.question,
                });
                let _ = tx.send(Ok(Event::default()
                    .event("awaiting_user")
                    .data(payload.to_string())
                )).await;
                // Persist the question as an assistant message so it survives a
                // page refresh and the agent can see its own ask in history.
                // The reply is appended by POST /api/sessions/{id}/ask.
                let question = ask.question.clone();
                if let Err(e) = state
                    .db
                    .add_message(&MessageRow::new(
                        ask.session_id,
                        "assistant",
                        question,
                        None,
                        None,
                        None,
                    ))
                    .await
                {
                    tracing::warn!(session_id = %ask.session_id, error = %e, "Failed to persist ask_user question");
                }
                // The tool is blocked on its oneshot — the /api/sessions/{id}/ask
                // endpoint resolves it when the user submits a reply.
            }
        }
    }

    // ── 7.5 Auto-continue: sub-agent results arrive → restart agent loop ──
    super::auto_continue::run_auto_continue(
        agent_yielded_for_subagents,
        &mut coord,
        &mut receivers,
        tx,
        &mut s,
        &pending_for_autocontinue,
        &mut messages_for_autocontinue,
        client_for_autocontinue,
        tools_for_autocontinue,
        &state,
        session_id,
        &proactivity,
        &meta_agent,
        &orchestrator,
        &assembled.hook_feedback,
        context_window_tokens,
        trace_id,
    )
    .await;

    // ── 8-11. Persist + close blocks + cleanup ───────────────────────
    let (total_in, total_out) = client_for_tokens.token_usage();
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
        total_in,
        total_out,
    )
    .await;

    // Session lifecycle: agent run completed successfully.
    orchestration::set_session_state(
        &state.db,
        session_id,
        everevo_core::types::SessionState::Completed,
    )
    .await;

    // ── Post-turn memory extraction + reflection (async, fire-and-forget) ──
    if !s.full_response.is_empty() {
        spawn_post_turn_tasks(&state, session_id, &req.message, &s.full_response).await;
    }

    Ok(())
}

// ── Post-turn background tasks ──────────────────────────────────────────────
// (extracted to post_turn.rs)

// ── Slash command handlers ─────────────────────────────────────────────────
// (extracted to slash_commands.rs)

// ── Helpers ────────────────────────────────────────────────────────────
// (extracted to helpers.rs)

/// Extract a human-readable message from a panic payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
