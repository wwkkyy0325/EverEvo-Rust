//! Auto-continue escalation loop — sub-agent results arrive → restart the
//! agent loop. Extracted from handler.rs during the 2026-08-13 physical
//! restructure.

use std::sync::Arc;

use axum::response::sse::Event;
use std::convert::Infallible;
use tokio::sync::mpsc;
use uuid::Uuid;

use everevo_core::llm::LlmMessage;
use everevo_db::models::MessageRow;

use crate::app_state::AppState;
use crate::orchestration::{ContentBlockStreamer, SessionCoordinator, SessionReceivers};

use super::wiring::apply_session_agent_wiring;

/// Guard against infinite restarts: max 5 auto-continue cycles, and break if
/// pending_subagents hasn't decreased between cycles.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_auto_continue(
    agent_yielded_for_subagents: bool,
    coord: &mut SessionCoordinator,
    receivers: &mut SessionReceivers,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    s: &mut ContentBlockStreamer,
    pending_for_autocontinue: &Arc<std::sync::atomic::AtomicUsize>,
    messages_for_autocontinue: &mut Vec<LlmMessage>,
    client_for_autocontinue: Arc<dyn everevo_core::LlmProvider>,
    tools_for_autocontinue: Arc<everevo_core::tool::ToolRegistry>,
    state: &Arc<AppState>,
    session_id: Uuid,
    proactivity: &Arc<std::sync::Mutex<everevo_agent::ProactivityState>>,
    meta_agent: &Option<Arc<std::sync::Mutex<everevo_agent::memory::MetaAgentState>>>,
    orchestrator: &Option<Arc<std::sync::Mutex<everevo_agent::loop_::MetaOrchestratorState>>>,
    hook_feedback: &Arc<std::sync::Mutex<Option<String>>>,
    context_window_tokens: usize,
    trace_id: Option<Uuid>,
) {
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
                let backlog = coord.backlog.lock().unwrap_or_else(|e| e.into_inner());
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
            // We require at least one sleep cycle before declaring stall to avoid
            // a race where pending was just set but results haven't arrived yet
            // (common with fast parallel_agents/team dispatch).
            if new_results.is_empty() {
                if pending >= last_pending && auto_cycles > 1 {
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
                let backlog = coord.backlog.lock().unwrap_or_else(|e| e.into_inner());
                backlog.len()
            };

            // ── Inject results and send SSE events ──
            for (task_id, desc, result) in &new_results {
                let short: String = result.chars().take(2000).collect();
                let _ = tx
                    .send(Ok(Event::default().event("subagent_result").data(
                        serde_json::json!({"id": task_id, "description": desc, "result": short})
                            .to_string(),
                    )))
                    .await;
                messages_for_autocontinue.push(everevo_core::llm::LlmMessage::user(format!(
                    "[SubAgent Result]\n{result}"
                )));
            }

            // ── Restart AgentRun with updated messages ──
            let mut agent2 = everevo_agent::AgentRun::new()
                .with_pending_subagents(Arc::clone(pending_for_autocontinue))
                .with_cancel_token(coord.cancel.clone())
                .with_compact_focus(coord.compact_focus.clone());
            // Same shared session wiring as the first run (apply_session_agent_wiring).
            agent2 = apply_session_agent_wiring(
                agent2,
                proactivity,
                meta_agent,
                orchestrator,
                hook_feedback,
                context_window_tokens,
                trace_id,
                state.telemetry_pipeline.clone(),
            );
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
                    Some(notif) = receivers.confirm_rx.recv() => {
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
                    Some(ask) = receivers.ask_user_rx.recv() => {
                        let payload = serde_json::json!({
                            "session_id": ask.session_id.to_string(),
                            "question": ask.question,
                        });
                        let _ = tx.send(Ok(Event::default()
                            .event("awaiting_user")
                            .data(payload.to_string())
                        )).await;
                        crate::orchestration::set_session_state(
                            &state.db,
                            ask.session_id,
                            everevo_core::types::SessionState::WaitingUser,
                        )
                        .await;
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
                            tracing::warn!(session_id = %ask.session_id, error = %e, "Failed to persist ask_user question (auto-continue)");
                        }
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
}
