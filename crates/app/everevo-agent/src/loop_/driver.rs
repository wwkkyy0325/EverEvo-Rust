//! The ReAct loop driver — `run_loop` plus its near-identical tool-result
//! deduplication helper.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

use everevo_core::llm::{LlmMessage, LlmProvider, LlmRole, StreamEvent, ToolSchema};
use everevo_core::tool::ToolRegistry;
use everevo_core::types::ToolCall;
use everevo_core::EverEvoError;
use everevo_core::{TelemetryEmitContext, TelemetryPipeline};

use super::convergence::{budget_line, convergence_stage, forced_final_prompt, Convergence};
use super::hooks::execute_with_hooks;
use super::proactivity::{hash_args, hash_str, ProactivityState};
use super::retrospective::{build_retrospective, truncate_for_retro};
use super::trim;
use super::AgentEvent;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) async fn run_loop(
    llm: &dyn LlmProvider,
    tools: &ToolRegistry,
    tool_schemas: &[ToolSchema],
    messages: &mut Vec<LlmMessage>,
    max_turns: usize,
    wall_clock_deadline: Option<std::time::Instant>,
    max_tool_result_chars: usize,
    max_context_chars: usize,
    confirmation: Option<&(dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync)>,
    telemetry: Option<&Arc<TelemetryPipeline>>,
    trace_id: Option<Uuid>,
    mut subagent_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    pending_subagents: &std::sync::atomic::AtomicUsize,
    tx: &mpsc::Sender<AgentEvent>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    compact_focus: &Option<Arc<std::sync::Mutex<Option<String>>>>,
    proactivity: &Option<Arc<std::sync::Mutex<ProactivityState>>>,
    meta_agent_state: &Option<Arc<std::sync::Mutex<crate::memory::meta_agent::MetaAgentState>>>,
    hook_feedback_slot: &Option<Arc<std::sync::Mutex<Option<String>>>>,
    compact_llm: Option<&dyn LlmProvider>,
    background: Option<&Arc<crate::context::BackgroundMaintenance>>,
    tool_cache_dir: Option<&Path>,
) -> Result<(), EverEvoError> {
    let mut turn = 0;
    // Bound the native-server-search truncation-continue retries across turns.
    let mut truncation_continues = 0;
    // Track the previous turn's tool signature for fixation detection.
    let mut prev_tool_sig: Option<(String, u64)> = None;
    // ── Run-level stats for the end-of-run retrospective ──────────
    let mut total_tool_calls = 0i32;
    let mut total_tool_success = 0i32;
    let mut failure_messages: Vec<String> = Vec::new();

    while max_turns == 0 || turn < max_turns {
        turn += 1;

        // ── Notify hooks of new turn (resets per-turn state) ──────
        for hook in &tools.hooks {
            hook.on_turn_start().await;
        }

        // Drain pending subagent results (non-blocking)
        if let Some(ref mut rx) = subagent_rx {
            while let Ok(result) = rx.try_recv() {
                messages.push(LlmMessage::user(format!("[SubAgent Result]\n{result}")));
            }
        }
        let turn_start = Instant::now();

        // ── Context management (Claude Code-aligned multi-layer) ────
        // Layer 0: Snip — zero-cost pruning of low-value tool results
        trim::snip_low_value_messages(messages);
        // Layer 1: Observation Masking — keep last N tool results, header older ones
        trim::mask_observations(messages);
        // Layer 2 (background): per-turn incremental rolling summary at the soft
        // threshold — non-blocking, writes only persisted state (spec rules
        // 5/6/7). Keeps the watermark low so Layer 3 rarely fires.
        let token_usage = trim::approx_tokens(messages.iter().map(|m| m.content.len()).sum());
        let token_limit = max_context_chars / 4;
        if let Some(bg) = background {
            use std::sync::atomic::Ordering;
            if !bg.in_flight.load(Ordering::Relaxed) && token_usage > (token_limit * 7) / 10
            // soft threshold 70%
            {
                bg.in_flight.store(true, Ordering::Relaxed);
                let bg = Arc::clone(bg);
                tokio::spawn(async move {
                    if let Err(e) = bg.maintain().await {
                        tracing::warn!(error = %e, "Background rolling-summary maintenance failed");
                    }
                    bg.in_flight.store(false, Ordering::Relaxed);
                });
                tracing::info!(
                    token_usage,
                    soft_limit = (token_limit * 7) / 10,
                    "Background rolling-summary maintenance spawned"
                );
            }
        }
        // Layer 3+4: Autocompact (LLM summarization) → Trim (hard drop fallback)
        // Trigger when approximate token count exceeds (context limit - buffer).
        // Uses the compaction model when configured, else the main model.
        if token_usage > token_limit.saturating_sub(trim::COMPACTION_BUFFER_TOKENS) {
            tracing::info!(token_usage, token_limit, "Context compaction triggered");
            // Read focus hint from CompactTool (if set), then clear it
            let focus = compact_focus.as_ref().and_then(|f| {
                let mut guard = f.lock().unwrap_or_else(|e| e.into_inner());
                guard.take()
            });
            let compact_model = compact_llm.unwrap_or(llm);
            if trim::autocompact(messages, max_context_chars, compact_model, focus.as_deref()).await
                == 0
            {
                trim::trim_context(messages, max_context_chars);
            }
        }

        tracing::info!(turn, msg_count = messages.len(), "Agent turn start");

        // ── 1. Call LLM with context overflow recovery ─────────────
        // Claude Code error recovery waterfall:
        //   1. Force emergency compaction → retry
        //   2. Force aggressive trim → retry
        //   3. Give up → propagate error to user
        let token_rx = match llm
            .stream_chat(messages, tool_schemas, cancel.cloned())
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                let err_str = e.to_string();
                let is_overflow = err_str.contains("context_length_exceeded")
                    || err_str.contains("prompt too long")
                    || err_str.contains("413")
                    || err_str.contains("too many tokens")
                    || err_str.contains("maximum context length");

                if is_overflow {
                    tracing::warn!(
                        error = %err_str,
                        msg_count = messages.len(),
                        "Context overflow detected — attempting emergency compaction"
                    );
                    // Waterfall step 1: aggressive trim (no API call needed)
                    let before = messages.len();
                    trim::trim_context(messages, max_context_chars / 2); // halve the budget
                    let after = messages.len();
                    tracing::info!(before, after, trimmed = before - after, "Emergency trim");

                    // Retry
                    llm.stream_chat(messages, tool_schemas, cancel.cloned())
                        .await
                        .map_err(|e2| {
                            let e2_str = e2.to_string();
                            tracing::error!(error = %e2_str, "Context overflow persists after emergency trim");
                            EverEvoError::Agent(format!(
                                "Context is too long even after emergency compaction. \
                                 Try using /compact or starting a new session. Detail: {e2_str}"
                            ))
                        })?
                } else {
                    return Err(e);
                }
            }
        };

        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut pending_tool: Option<(String, String, String)> = None;
        let mut saw_server_tool = false;
        let mut last_stop_reason: Option<String> = None;

        let mut token_rx = token_rx;
        loop {
            // Stall guard — mirror the sub-agent loop's 120s per-event timeout so
            // a hung LLM stream can't block the main loop indefinitely.
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(120), token_rx.recv()).await;
            let event = match event {
                Ok(Some(e)) => e,
                Ok(None) => break, // channel closed
                Err(_elapsed) => {
                    let msg = "LLM stream stalled (no events for 120s)".to_string();
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(EverEvoError::Agent(msg));
                }
            };
            match event {
                StreamEvent::Thinking(t) => {
                    current_thinking.push_str(&t);
                    let _ = tx.send(AgentEvent::Thinking(t)).await;
                }
                StreamEvent::Text(t) => {
                    current_text.push_str(&t);
                    let _ = tx.send(AgentEvent::TextDelta(t)).await;
                }
                StreamEvent::ToolCallStart { id, name } => {
                    pending_tool = Some((id, name, String::new()));
                }
                StreamEvent::ToolCallArg { id, arg_delta } => {
                    if let Some((ref pending_id, _, ref mut args)) = pending_tool {
                        if pending_id == &id {
                            args.push_str(&arg_delta);
                        }
                    }
                }
                StreamEvent::ServerToolUse { .. } => {
                    // Provider-executed tool (native web search) — the provider
                    // runs it within this turn; nothing to dispatch.
                    saw_server_tool = true;
                }
                StreamEvent::Done { stop_reason, .. } => {
                    last_stop_reason = stop_reason;
                    if let Some((id, name, args_str)) = pending_tool.take() {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
                        let _ = tx
                            .send(AgentEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                            .await;
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                    break;
                }
                StreamEvent::Error(msg) => {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(EverEvoError::LlmProvider(msg));
                }
            }
        }

        // Native server-side search truncated (stop_reason=max_tokens): continue
        // the turn with the partial context instead of emitting a premature Done.
        // Server blocks are intentionally NOT replayed (an incomplete
        // `server_tool_use` in history makes the API reject with 400).
        if tool_calls.is_empty()
            && saw_server_tool
            && last_stop_reason.as_deref() == Some("max_tokens")
            && truncation_continues < 4
        {
            truncation_continues += 1;
            let thinking = if current_thinking.is_empty() {
                None
            } else {
                Some(current_thinking.clone())
            };
            messages.push(LlmMessage {
                role: LlmRole::Assistant,
                content: current_text.clone(),
                thinking,
                tool_calls: None,
                tool_call_id: None,
                images: Vec::new(),
            });
            tracing::info!(
                truncation_continues,
                "Native server-side search truncated (max_tokens) — continuing turn"
            );
            continue;
        }

        // If text but no tool calls → check for pending sub-agents first.
        if tool_calls.is_empty() {
            let pending = pending_subagents.load(std::sync::atomic::Ordering::SeqCst);
            if pending > 0 {
                if let Some(ref mut rx) = subagent_rx {
                    while let Ok(result) = rx.try_recv() {
                        messages.push(LlmMessage::user(format!("[SubAgent Result]\n{result}")));
                    }
                }
                tracing::info!(pending, "LLM says Done but sub-agents running — yielding");
                if !current_text.is_empty() {
                    let _ = tx.send(AgentEvent::TextDelta(current_text.clone())).await;
                }
                let _ = tx.send(AgentEvent::WaitingForSubAgents { pending }).await;
                return Ok(());
            }

            // The model produced reasoning (thinking) but NO text and NO tool
            // call — it never committed an answer. Committing an empty
            // final_text turns the whole turn into an empty prediction, which
            // the GAIA harness scores as a FAIL (Q3/Q37/Q46 in run-4 died
            // exactly here: brute-force enumeration in thinking burned the
            // budget without ever emitting a value). Instead, push the
            // reasoning back and run ONE no-tool convergence call so a value
            // actually gets committed — an over-confident guess beats a silent
            // empty answer, and the model's own reasoning is preserved as the
            // seed. No sub-agent yield, no early return with empty text.
            if current_text.is_empty() && !current_thinking.is_empty() {
                tracing::warn!(
                    turn,
                    thinking_chars = current_thinking.len(),
                    "LLM produced reasoning but no answer — forcing terminal convergence"
                );
                messages.push(LlmMessage {
                    role: LlmRole::Assistant,
                    content: String::new(),
                    thinking: Some(current_thinking.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    images: Vec::new(),
                });
                messages.push(LlmMessage::user(forced_final_prompt()));
                let final_text = match llm.chat(messages, &[]).await {
                    Ok(resp) => resp.content.unwrap_or_default(),
                    Err(e) => {
                        tracing::warn!(error = %e, "Forced convergence call failed");
                        String::new()
                    }
                };
                let summary = build_retrospective(
                    turn as i32,
                    total_tool_calls,
                    total_tool_success,
                    &failure_messages,
                );
                let _ = tx.send(AgentEvent::Retrospective { summary }).await;
                let _ = tx.send(AgentEvent::Done { final_text }).await;
                return Ok(());
            }

            // No pending sub-agents → truly done.
            let final_text = current_text.clone();
            let summary = build_retrospective(
                turn as i32,
                total_tool_calls,
                total_tool_success,
                &failure_messages,
            );
            let _ = tx.send(AgentEvent::Retrospective { summary }).await;
            let _ = tx.send(AgentEvent::Done { final_text }).await;
            return Ok(());
        }

        // ── 2. Build assistant message with tool calls ──────────────
        let thinking = if current_thinking.is_empty() {
            None
        } else {
            Some(current_thinking.clone())
        };
        let assistant_msg = LlmMessage {
            role: LlmRole::Assistant,
            content: if current_text.is_empty() {
                String::new()
            } else {
                current_text.clone()
            },
            thinking,
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            images: Vec::new(),
        };
        messages.push(assistant_msg);

        // ── 3. Execute tools ────────────────────────────────────────
        let mut tool_result_pairs: Vec<(String, String, Vec<everevo_core::ImageData>)> = Vec::new();
        let mut tool_calls_success = 0i32;

        for tc in &tool_calls {
            total_tool_calls += 1;
            let tool = tools.get(&tc.name);
            if let Some(confirm_fn) = confirmation {
                if !confirm_fn(&tc.name, &tc.arguments) {
                    let skip_msg = format!("User declined execution of tool '{}'", tc.name);
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: skip_msg.clone(),
                            is_error: true,
                            images: Vec::new(),
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), skip_msg, Vec::new()));
                    continue;
                }
            }

            let result = match tool {
                Some(tool) => {
                    // Per-tool timeout: 300s for shell/build, 120s default.
                    // Prevents hung tools from blocking the agent loop indefinitely.
                    let timeout_secs = if tc.name == "shell" || tc.name.contains("build") {
                        300u64
                    } else {
                        120u64
                    };
                    let exec_fut = execute_with_hooks(
                        tool.as_ref(),
                        &tc.name,
                        &tc.arguments,
                        None,
                        &tools.hooks,
                    );
                    let result = match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_secs),
                        exec_fut,
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_elapsed) => Err(EverEvoError::Tool {
                            tool: tc.name.clone(),
                            message: format!("Timed out after {timeout_secs}s"),
                        }),
                    };
                    // Report hook blocks via SSE
                    if let Err(ref e) = result {
                        if e.to_string().contains("blocked") {
                            let _ = tx
                                .send(AgentEvent::ToolCallEnd {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    content: format!("Tool blocked: {e}"),
                                    is_error: true,
                                    images: Vec::new(),
                                })
                                .await;
                            failure_messages.push(format!("{}: blocked", tc.name));
                            tool_result_pairs.push((
                                tc.id.clone(),
                                format!("Tool blocked: {e}"),
                                Vec::new(),
                            ));
                            continue;
                        }
                    }
                    result
                }
                None => Err(EverEvoError::Tool {
                    tool: tc.name.to_string(),
                    message: "Unknown tool".into(),
                }),
            };

            match result {
                Ok(output) => {
                    // Large tool outputs are paged to disk (spec deliverable 6):
                    // the context keeps a 2KB preview + absolute path, and the
                    // full text is retrievable via the `tool_cache_read` tool.
                    let truncated = match trim::page_tool_output(
                        &tc.name,
                        &tc.id,
                        &output.content,
                        tool_cache_dir,
                    )
                    .await
                    {
                        Some(paged) => paged,
                        None => trim::truncate_output(&output.content, max_tool_result_chars),
                    };
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: truncated.clone(),
                            is_error: output.is_error,
                            images: output.images.clone(),
                        })
                        .await;
                    if output.is_error {
                        if tc.name == "shell" && truncated.contains("确认")
                            || truncated.contains("confirmation")
                        {
                            let _ = tx
                                .send(AgentEvent::ConfirmationNeeded {
                                    command: tc
                                        .arguments
                                        .get("command")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    reason: truncated.clone(),
                                })
                                .await;
                        }
                        failure_messages.push(format!(
                            "{}: {}",
                            tc.name,
                            truncate_for_retro(&truncated)
                        ));
                        tracing::warn!(tool = %tc.name, "Tool returned error");
                    } else {
                        tool_calls_success += 1;
                        total_tool_success += 1;
                    }
                    tool_result_pairs.push((tc.id.clone(), truncated, output.images.clone()));
                }
                Err(e) => {
                    let err_msg = format!("Tool execution failed: {e}");
                    failure_messages.push(format!("{}: {err_msg}", tc.name));
                    let _ = tx
                        .send(AgentEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: err_msg.clone(),
                            is_error: true,
                            images: Vec::new(),
                        })
                        .await;
                    tool_result_pairs.push((tc.id.clone(), err_msg, Vec::new()));
                }
            }
        }

        // ── 3.5 Deduplicate near-identical tool results ─────────────
        // When N sub-agents/tools return the SAME observation (e.g.
        // "list_dir vs shell path inconsistency"), pushing all N results
        // floods the context with duplicates → model loops its thinking.
        if tool_result_pairs.len() > 3 {
            let original = tool_result_pairs.len();
            deduplicate_tool_results(&mut tool_result_pairs);
            if tool_result_pairs.len() < original {
                // At least one group was collapsed — log the reduction.
            }
        }

        // ── 4. Merge tool results into ONE user message ─────────────
        if !tool_result_pairs.is_empty() {
            if tool_result_pairs.len() == 1 {
                let (id, content, images) =
                    tool_result_pairs.into_iter().next().unwrap_or_default();
                let mut msg = LlmMessage::tool(&content, &id);
                if !images.is_empty() {
                    msg.images = images;
                }
                messages.push(msg);
            } else {
                let ids: Vec<String> = tool_result_pairs
                    .iter()
                    .map(|(id, _, _)| id.clone())
                    .collect();
                let all_images: Vec<_> = tool_result_pairs
                    .iter()
                    .flat_map(|(_, _, imgs)| imgs.clone())
                    .collect();
                let payload = serde_json::to_string(
                    &tool_result_pairs
                        .iter()
                        .map(|(id, content, _)| serde_json::json!({"i": id, "c": content}))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default();
                let ids_joined = ids.join("|");
                let mut msg = LlmMessage::tool(&payload, &ids_joined);
                msg.tool_call_id = Some(ids_joined);
                if !all_images.is_empty() {
                    msg.images = all_images;
                }
                messages.push(msg);
            }
        }

        // ── 4.4 Hook feedback: read ReflectGateHook feedback ──────
        if let Some(ref slot) = hook_feedback_slot {
            let mut fb = slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(feedback) = fb.take() {
                messages.push(LlmMessage::user(format!("[TOOL FEEDBACK]\n{feedback}")));
                tracing::debug!(feedback_len = feedback.len(), "Hook feedback injected");
            }
        }

        // ── 4.5 Meta-Agent: inject pending hint at turn start ──────
        if let Some(ref meta_state) = meta_agent_state {
            let mut ms = meta_state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hint) = ms.take_hint() {
                messages.push(LlmMessage::user(format!("[META-AGENT HINT]\n{hint}")));
                tracing::debug!(hint_len = hint.len(), "Meta-agent hint injected");
            }
        }

        // ── 4.6 Proactivity: detect fixation and inject intervention ─
        if let Some(ref state) = proactivity {
            // Collect first-tool info for this turn's fixation tracking.
            let this_tool = tool_calls
                .first()
                .map(|tc| (tc.name.clone(), hash_args(&tc.arguments)));
            // Determine if this turn had any error.
            let has_error = (tool_calls_success as usize) < tool_calls.len();

            if let Some((ref name, args_h)) = this_tool {
                let prev_sig = prev_tool_sig.as_ref().map(|(n, h)| (n.as_str(), *h));
                let mut ps = state.lock().unwrap_or_else(|e| e.into_inner());
                ps.update(name, has_error, args_h, prev_sig);

                // Track web_search / web_fetch usage to mark research done.
                if name == "web_search" || name == "web_fetch" {
                    ps.mark_researched();
                }

                // Inject intervention message if escalation triggered.
                if let Some(intervention) = ps.intervention_message() {
                    messages.push(LlmMessage::user(&intervention));
                }

                prev_tool_sig = Some((name.clone(), args_h));
            }
        }

        // ── 4.7 Meta-Agent: trigger on interval or degradation ─────
        if let Some(ref meta) = meta_agent_state {
            let mut ms = meta.lock().unwrap_or_else(|e| e.into_inner());
            ms.increment_turn();
            let escalation = proactivity
                .as_ref()
                .map(|p| {
                    let ps = p.lock().unwrap_or_else(|e| e.into_inner());
                    ps.level as u32
                })
                .unwrap_or(0);
            if ms.should_trigger(escalation) && ms.has_llm() {
                ms.mark_triggered();
                // Fire-and-forget: spawn meta-diagnosis in background
                if let Some(ref llm) = ms.llm {
                    let llm = Arc::clone(llm);
                    let fm = ms.fact_manager.clone();
                    let meta_state = Arc::clone(meta);
                    // Build a summary of recent messages for the prompt
                    let recent_summary = messages
                        .iter()
                        .rev()
                        .take(10)
                        .map(|m| {
                            let role = match m.role {
                                everevo_core::llm::LlmRole::User => "U",
                                everevo_core::llm::LlmRole::Assistant => "A",
                                _ => "S",
                            };
                            let content = if m.content.chars().count() > 100 {
                                let truncated: String = m.content.chars().take(100).collect();
                                format!("{truncated}…")
                            } else {
                                m.content.clone()
                            };
                            format!("[{role}] {content}")
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    tokio::spawn(async move {
                        let hint = crate::memory::meta_agent::meta_diagnose(
                            &llm,
                            fm.as_deref(),
                            &crate::memory::paradigm::TrajectoryBuffer::default(),
                            escalation,
                            &recent_summary,
                        )
                        .await;
                        if let Some(h) = hint {
                            let mut ms = meta_state.lock().unwrap_or_else(|e| e.into_inner());
                            ms.set_hint(h);
                        }
                    });
                }
            }
        }

        // ── 5. Emit turn complete ───────────────────────────────────
        let _ = tx.send(AgentEvent::TurnComplete).await;

        if let Some(telemetry) = telemetry {
            let turn_error = (tool_calls_success as usize) < tool_calls.len();
            let (error_type, error_message) = if turn_error {
                let failed = tool_calls.len() as i32 - tool_calls_success;
                (
                    Some("tool_error".to_string()),
                    Some(format!(
                        "{failed} of {} tool calls failed",
                        tool_calls.len()
                    )),
                )
            } else {
                (None, None)
            };
            telemetry.emit(&TelemetryEmitContext {
                trace_id,
                turn_number: Some(turn as i32),
                tool_calls_total: Some(tool_calls.len() as i32),
                tool_calls_success: Some(tool_calls_success),
                task_completed: Some(false),
                turn_latency_ms: Some(turn_start.elapsed().as_millis() as i64),
                error_type,
                error_message,
                ..Default::default()
            });
        }

        // ── 6. Check if we should inject a reminder ─────────────────
        if let Some(deadline) = wall_clock_deadline {
            // Benchmark mode: escalating convergence nudges + a per-turn budget
            // line so the model feels both turn and wall-clock pressure.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let wall_frac = (remaining.as_secs_f64() / 300.0).clamp(0.0, 1.0);
            match convergence_stage(turn, max_turns, wall_frac) {
                Convergence::Commit => {
                    messages.push(LlmMessage::user(
                        "⏰ Deadline: STOP exploring. Do NOT start new research, do NOT write \
                         plans or code. Your very next response MUST end with a single \
                         `Final answer:` line containing ONLY the value — best-effort beats \
                         no answer, and an uncertain value extracted from what you already \
                         found beats narration.",
                    ));
                }
                Convergence::Converge => {
                    messages.push(LlmMessage::user(
                        "⏰ Time check: start converging. Commit to the answer you believe \
                         best from what you already gathered, stop new exploration, and \
                         prepare a single `Final answer:` line.",
                    ));
                }
                Convergence::None => {}
            }
            let turns_left = if max_turns > 0 {
                Some(max_turns.saturating_sub(turn))
            } else {
                None
            };
            messages.push(LlmMessage::user(budget_line(
                turns_left,
                Some(remaining.as_secs()),
            )));
        } else if max_turns > 0 && turn >= max_turns - 2 && turn < max_turns {
            messages.push(LlmMessage::user(
                "You have only a few turns remaining. Please provide your final answer now.",
            ));
        }
    }

    if max_turns > 0 {
        if wall_clock_deadline.is_some() {
            // Benchmark forced terminal commit: one last no-tool LLM call for
            // ONLY the final answer, seeded from the full conversation (which
            // holds the model's own prior committed text), then emit Done so the
            // harness scorer / re-prompt sees a final_text instead of an error.
            messages.push(LlmMessage::user(forced_final_prompt()));
            let final_text = match llm.chat(messages, &[]).await {
                Ok(resp) => resp.content.unwrap_or_default(),
                Err(_) => String::new(),
            };
            let _ = tx.send(AgentEvent::Done { final_text }).await;
        } else {
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!(
                        "Max turns ({max_turns}) reached. Please try a simpler request."
                    ),
                })
                .await;
        }
    }

    Ok(())
}

// ── Tool Result Deduplication ──────────────────────────────────────────────

/// When N tool results in the same turn are near-identical (e.g. 3 sub-agents
/// all reporting the same path inconsistency bug), keep the first 2 and replace
/// the rest with a collapsed summary. This prevents flooding the LLM context
/// with duplicate observations that cause repetition loops in the thinking output.
fn deduplicate_tool_results(results: &mut [(String, String, Vec<everevo_core::ImageData>)]) {
    if results.len() < 3 {
        return;
    }

    // Phase 1: fingerprint each result
    let fingerprints: Vec<u64> = results
        .iter()
        .map(|(_, content, _)| {
            let prefix: String = content.chars().take(200).collect();
            hash_str(&prefix)
        })
        .collect();

    // Phase 2: find groups with high similarity
    let mut seen: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    for (i, &fp) in fingerprints.iter().enumerate() {
        seen.entry(fp).or_default().push(i);
    }

    // Phase 3: collapse groups with >2 members
    for indices in seen.values() {
        if indices.len() <= 2 {
            continue;
        }
        let keep_id = results[indices[0]].0.clone();
        let dup_count = indices.len() - 2;
        for &idx in &indices[2..] {
            results[idx] = (
                results[idx].0.clone(),
                format!(
                    "(duplicate of {keep_id} — {dup_count} similar results collapsed to save context)"
                ),
                Vec::new(),
            );
        }
    }
}
