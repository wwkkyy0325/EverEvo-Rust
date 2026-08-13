//! The ReAct loop driver — `run_loop` plus its near-identical tool-result
//! deduplication helper.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use everevo_core::llm::{LlmMessage, LlmProvider, LlmRole, ToolSchema};
use everevo_core::tool::ToolRegistry;
use everevo_core::EverEvoError;

use super::classify::{classify_tool, ToolKind};
use super::convergence::{
    budget_line, convergence_stage, forced_final_prompt, verified_deadline_prompt,
    verified_wrapup_prompt, Convergence, POST_VERIFY_STALL_TURNS,
};
use super::dedup::deduplicate_tool_results;
use super::hooks::execute_with_hooks;
use super::proactivity::hash_args;
use super::retrospective::{build_retrospective, truncate_for_retro};
use super::state::{is_terminal, transition, LoopEvent, LoopState};
use super::trim;
use super::AgentEvent;

use crate::stages::{classify, Difficulty};

/// Re-prompt injected when a HARD question tries to commit without any
/// verification having run. The loop enforces this (research: verification
/// concentrates its value on hard questions — Leni arXiv 2607.17044).
const VERIFY_BEFORE_COMMIT_PROMPT: &str = "\
You are about to commit a final answer to a COMPLEX question, but no \
verification has been run yet. Before you emit `Final answer:`, you MUST run \
a verification step:
1. Run `python verify_candidate.py verify --answer <candidate> --expected \
<derived value>` (with --unit/--compute/--expect-list/--entity as \
applicable). If it reports violations, repair the candidate (at most 2 \
repairs). If it passes, you may commit.
2. If the deterministic check still disagrees, run `cluster verify` with \
skeptical reviewer perspectives (e.g. \"numeric reviewer\", \
\"source-verbatim reviewer\") on the candidate, and commit only if it \
survives.
Do not emit `Final answer:` until at least one verification step has run.";

#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    // `state` is recorded at every decision point for traceability; terminal
    // transitions return immediately and the loop top re-enters Observe, so
    // several assignments are never read by the compiler. That is by design.
    unused_assignments
)]
pub(crate) async fn run_loop(
    llm: &dyn LlmProvider,
    tools: &ToolRegistry,
    tool_schemas: &[ToolSchema],
    messages: &mut Vec<LlmMessage>,
    config: super::config::RunConfig,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<(), EverEvoError> {
    // Destructure the owned config into the same-named locals the loop body
    // already references — the body is unchanged; entry points now configure
    // once via RunConfig instead of re-assembling ~20 positional params
    // (unified-entry refactor, architecture-restructure-plan.md P0).
    let max_turns = config.max_turns;
    let wall_clock_deadline = config.wall_clock_deadline;
    let max_tool_result_chars = config.max_tool_result_chars;
    let max_context_chars = config.max_context_chars;
    let confirmation = config.confirmation.as_deref();
    let telemetry = config.telemetry.as_ref();
    let trace_id = config.trace_id;
    let mut subagent_rx = config.subagent_rx;
    let pending_subagents = &config.pending_subagents;
    let cancel = config.cancel.as_ref();
    let compact_focus = &config.compact_focus;
    let proactivity = &config.proactivity;
    let meta_agent_state = &config.meta_agent_state;
    let hook_feedback_slot = &config.hook_feedback_slot;
    let compact_llm = config.compact_llm.as_deref();
    let background = config.background.as_ref();
    let tool_cache_dir = config.tool_cache_dir.as_deref();
    let verify_gate = config.verify_gate;

    let mut turn = 0;
    // Bound the native-server-search truncation-continue retries across turns.
    let mut truncation_continues = 0;
    // Track the previous turn's tool signature for fixation detection.
    let mut prev_tool_sig: Option<(String, u64)> = None;
    // ── Run-level stats for the end-of-run retrospective ──────────
    let mut total_tool_calls = 0i32;
    let mut total_tool_success = 0i32;
    let mut failure_messages: Vec<String> = Vec::new();

    // ── Adaptive verification: per-question difficulty + enforcement ──
    // Classified ONCE from the current request's message (the last user
    // message of the freshly-assembled context — before any tool results are
    // appended). Simple questions skip the verification commit gate entirely;
    // hard questions must run a verification step before committing.
    let difficulty = messages
        .iter()
        .rev()
        .find(|m| m.role == LlmRole::User)
        .map(|m| classify(&m.content))
        .unwrap_or(Difficulty::Hard);
    // A question already carries a verification step (e.g. a resumed
    // auto-continue run) if any past assistant tool call was a verifier.
    let mut verified = messages.iter().any(|m| match &m.tool_calls {
        Some(calls) => calls
            .iter()
            .any(|tc| classify_tool(&tc.name, &tc.arguments) == ToolKind::Verifier),
        None => false,
    });
    // Whether the agent finalized a structural problem model (causal draft) on
    // a hard question — informational for the commit-gate re-prompt.
    let mut model_drafted = messages.iter().any(|m| match &m.tool_calls {
        Some(calls) => calls
            .iter()
            .any(|tc| classify_tool(&tc.name, &tc.arguments) == ToolKind::ProblemModelFinalize),
        None => false,
    });
    let mut verify_reprompts = 0u32;
    // Turns spent on NON-verification tool calls after a verification step ran.
    // Tracks the "verified candidate exists but the agent keeps exploring"
    // stall (the dominant GAIA timeout mode) so the runtime can nudge a commit.
    let mut post_verify_turns = 0usize;
    let mut post_verify_nudged = false;

    // ── Explicit loop state machine (see loop_/state.rs + agent-states.md) ──
    let mut state = LoopState::Init;
    state = transition(state, LoopEvent::Ready).0; // T1 Init → Observe

    while !is_terminal(state) && (max_turns == 0 || turn < max_turns) {
        turn += 1;

        // T16 cancellation check (gap fix): a cancel between turns must stop
        // the loop rather than waiting for the next in-flight LLM/tool call
        // to notice. Previously only run_subagent checked between calls.
        if cancel.is_some_and(|c| c.is_cancelled()) {
            state = transition(state, LoopEvent::Cancel).0;
            let _ = tx
                .send(AgentEvent::Error {
                    message: "Cancelled".into(),
                })
                .await;
            return Ok(());
        }
        // Each loop iteration re-enters the Observe state (T10/T11/T12 feed
        // back here from Act / Verify / normal continuation).
        state = LoopState::Observe;

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

        // ── 1. Call LLM with context overflow recovery (llm_call.rs) ──
        state = transition(state, LoopEvent::Ready).0; // T2 Observe → Solve
        let token_rx = match super::llm_call::call_llm_with_overflow_recovery(
            llm,
            messages,
            tool_schemas,
            cancel,
            max_context_chars,
        )
        .await
        {
            Ok(rx) => rx,
            Err(le) => {
                state = transition(
                    state,
                    if le.overflow {
                        LoopEvent::Overflow // T4 → Error
                    } else {
                        LoopEvent::StreamFailure // T3 → Error
                    },
                )
                .0;
                return Err(le.error);
            }
        };

        let mut token_rx = token_rx;
        // ── 2. Drain the token stream (token_stream.rs) ──
        let stream_accum = match super::token_stream::process_token_stream(&mut token_rx, tx).await
        {
            Ok(accum) => accum,
            Err(e) => {
                state = transition(state, LoopEvent::StreamFailure).0; // T3 → Error
                return Err(e);
            }
        };
        let super::token_stream::StreamAccum {
            current_text,
            current_thinking,
            tool_calls,
            saw_server_tool,
            last_stop_reason,
        } = stream_accum;

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
            state = transition(state, LoopEvent::Truncated).0; // T19 Solve self-loop
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
                state = transition(state, LoopEvent::SubAgentsPending).0; // T6 → WaitSubAgents
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
                state = transition(state, LoopEvent::ThinkingOnly).0; // T7 → Converge
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
                state = transition(state, LoopEvent::Ready).0; // T14 Converge → Done
                let _ = tx.send(AgentEvent::Retrospective { summary }).await;
                let _ = tx.send(AgentEvent::Done { final_text }).await;
                return Ok(());
            }

            // ── Verification commit gate (hard questions only) ──────
            // If a hard question commits without ANY verification step and
            // turns/time remain, re-prompt once to force verification. Capped
            // (2 re-prompts) so a stuck model still commits best-effort
            // rather than looping forever. Simple questions never gate.
            if difficulty == Difficulty::Hard && !verified && verify_reprompts < 2 && verify_gate {
                let budget_ok = match wall_clock_deadline {
                    Some(deadline) => {
                        deadline
                            .saturating_duration_since(std::time::Instant::now())
                            .as_secs()
                            > 30
                    }
                    None => turn < max_turns.saturating_sub(1),
                };
                if budget_ok {
                    verify_reprompts += 1;
                    state = transition(state, LoopEvent::UnverifiedHard).0; // T8 → Verify
                    tracing::info!(
                        turn,
                        reprompt = verify_reprompts,
                        model_drafted,
                        "Hard question committed unverified — re-prompting for verification"
                    );
                    messages.push(LlmMessage::user(VERIFY_BEFORE_COMMIT_PROMPT));
                    // If the agent never built a structural problem model either,
                    // suggest it for complex/compound questions (informational —
                    // modeling is encouraged, not enforced).
                    if !model_drafted {
                        messages.push(LlmMessage::user(
                            "For a COMPLEX question, consider building a structural problem \
                             model first via `problem_model` (sub-questions + epistemic \
                             status + causal/evidence edges), then answer from it.",
                        ));
                    }
                    continue;
                }
            }

            // No pending sub-agents → truly done.
            state = transition(state, LoopEvent::DoneSignal).0; // T9 → Done
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
        state = transition(state, LoopEvent::ToolCalls).0; // T5 Solve → Act
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

        let mut turn_had_verify = false;
        for tc in &tool_calls {
            total_tool_calls += 1;
            // Any verification step (deterministic sandbox verifier or
            // adversarial cluster verify) satisfies the hard-question gate.
            if classify_tool(&tc.name, &tc.arguments) == ToolKind::Verifier {
                verified = true;
                turn_had_verify = true;
            }
            // A finalized problem model (causal draft) is a modeling signal.
            if classify_tool(&tc.name, &tc.arguments) == ToolKind::ProblemModelFinalize {
                model_drafted = true;
            }
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
                    let exec_fut = execute_with_hooks(
                        tool.as_ref(),
                        &tc.name,
                        &tc.arguments,
                        cancel,
                        &tools.hooks,
                    );
                    // ask_user is exempt from the per-tool timeout: it blocks
                    // until the user replies (Claude Code style) and is only
                    // terminated by a reply, SSE disconnect, or /interrupt.
                    // Everything else: 300s for shell/build, 120s default.
                    let result = if tc.name == "ask_user" {
                        exec_fut.await
                    } else {
                        let timeout_secs = if tc.name == "shell" || tc.name.contains("build") {
                            300u64
                        } else {
                            120u64
                        };
                        match tokio::time::timeout(
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
                        }
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
                None => {
                    // Helpful, recoverable error: list available tools so the
                    // agent can pick a valid one instead of repeating the call.
                    let mut available = tools.names();
                    available.sort_unstable();
                    Err(EverEvoError::Tool {
                        tool: tc.name.to_string(),
                        message: format!(
                            "Unknown tool `{}`. Available: {}",
                            tc.name,
                            available.join(", ")
                        ),
                    })
                }
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

        // ── 3.25 Post-verified stall counter ───────────────────────
        // A verification step already ran and this turn's tool calls were NOT
        // another verification → the agent may be re-exploring after owning a
        // verified candidate. Count it so the convergence region can nudge a
        // commit (anti-verification-spiral; see POST_VERIFY_STALL_TURNS).
        if verified && !tool_calls.is_empty() && !turn_had_verify {
            post_verify_turns += 1;
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
            // Adaptive: only trigger the autonomous meta-diagnosis on hard
            // questions — simple sessions pay no self-diagnosis overhead.
            if difficulty == Difficulty::Hard && ms.should_trigger(escalation) && ms.has_llm() {
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

        // ── 5. Emit turn complete (retrospective.rs) ─────────────────
        super::retrospective::emit_turn_complete(
            tx,
            telemetry,
            trace_id,
            turn,
            tool_calls.len(),
            tool_calls_success,
            turn_start,
        )
        .await;

        // ── 6. Check if we should inject a reminder ─────────────────
        if let Some(deadline) = wall_clock_deadline {
            // Benchmark mode: escalating convergence nudges + a per-turn budget
            // line so the model feels both turn and wall-clock pressure.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            // Normalize against the ACTUAL deadline length (mirrors mod.rs's
            // EVEREVO_BENCHMARK_WALLCLOCK) so the nudges fire proportionally —
            // a hardcoded /300 makes them relative to only the last 300s and
            // fires them too early on a long (1800s) run.
            let wall_total: f64 = std::env::var("EVEREVO_BENCHMARK_WALLCLOCK")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0.0)
                .unwrap_or(300.0);
            let wall_frac = (remaining.as_secs_f64() / wall_total).clamp(0.0, 1.0);
            // Convergence escalation and the verification spiral are FORMAL FSM
            // states, routed through transition() so the state machine is the
            // single source of truth for the agent's behavior (agent-states.md
            // T21-T26). Verified-aware prompts name the value the model already
            // owns instead of generic wall-clock pressure.
            let verified_aware = post_verify_turns > 0;
            match convergence_stage(turn, max_turns, wall_frac) {
                Convergence::Commit => {
                    state = transition(state, LoopEvent::BudgetCommit).0; // T25 → Escalating
                    let prompt = if verified_aware {
                        verified_deadline_prompt().to_string()
                    } else {
                        "⏰ Deadline: STOP exploring. Do NOT start new research, do NOT write \
                         plans or code. Your very next response MUST end with a single \
                         `Final answer:` line containing ONLY the value — best-effort beats \
                         no answer, and an uncertain value extracted from what you already \
                         found beats narration."
                            .to_string()
                    };
                    messages.push(LlmMessage::user(prompt));
                }
                Convergence::Converge => {
                    state = transition(state, LoopEvent::BudgetConverge).0; // T23 → Escalating
                    let prompt = if verified_aware {
                        verified_wrapup_prompt(post_verify_turns)
                    } else {
                        "⏰ Time check: CONVERGE NOW. After at most 2 more tool calls, you MUST \
                         stop exploring and commit the single best value you have on a \
                         `Final answer:` line. Do not start new research threads; if a \
                         candidate is close, verify or commit it — an uncertain verified \
                         value beats an empty timeout."
                            .to_string()
                    };
                    messages.push(LlmMessage::user(prompt));
                }
                Convergence::None => {
                    // No wall-clock escalation yet → proactive anti-verification-spiral
                    // nudge (once, T21 → Stalled): a verified candidate exists but the
                    // agent keeps making non-verification tool calls.
                    if !post_verify_nudged && post_verify_turns >= POST_VERIFY_STALL_TURNS {
                        state = transition(state, LoopEvent::VerifiedStalled).0; // T21 → Stalled
                        post_verify_nudged = true;
                        messages.push(LlmMessage::user(verified_wrapup_prompt(post_verify_turns)));
                    }
                }
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
            // Wall-clock nearly exhausted — the harness kills the request at
            // ~300s, so break out and let the forced terminal commit below run
            // a no-tool extraction call instead of leaving an empty prediction.
            // Slow local models (e.g. Qwen3.5-2B) spend 15-25s per turn, never
            // reach max_turns within the deadline, and ignore the convergence
            // nudges above — this is the only path that guarantees a value.
            if max_turns > 0 && remaining.as_secs() <= 30 {
                state = transition(state, LoopEvent::WallClockLow).0; // T18 → TerminalCommit
                break;
            }
        } else if max_turns > 0 && turn >= max_turns - 2 && turn < max_turns {
            messages.push(LlmMessage::user(
                "You have only a few turns remaining. Please provide your final answer now.",
            ));
        }
    }

    if max_turns > 0 {
        if wall_clock_deadline.is_some() {
            // T18 forced terminal commit: one last no-tool LLM call for ONLY the
            // final answer, seeded from the full conversation (which holds the
            // model's own prior committed text), then emit Done so the harness
            // scorer / re-prompt sees a final_text instead of an error.
            state = transition(state, LoopEvent::WallClockLow).0; // → TerminalCommit
            messages.push(LlmMessage::user(forced_final_prompt()));
            let final_text = match llm.chat(messages, &[]).await {
                Ok(resp) => resp.content.unwrap_or_default(),
                Err(_) => String::new(),
            };
            state = transition(state, LoopEvent::Ready).0; // TerminalCommit → Done
            let _ = tx.send(AgentEvent::Done { final_text }).await;
        } else {
            state = transition(state, LoopEvent::TurnsExhausted).0; // T17 → Error
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
