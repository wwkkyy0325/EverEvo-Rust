//! Token-stream event processing for one LLM turn — extracted from driver.rs
//! during the 2026-08-13 physical restructure.
//!
//! Consumes the streamed events (thinking / text / tool calls) from the LLM
//! provider, accumulates them, and re-emits them as [`AgentEvent`]s to the
//! caller's SSE channel. On stream stall or provider error, sends an
//! [`AgentEvent::Error`] and returns the error (the caller applies the T3
//! StreamFailure transition).

use everevo_core::llm::StreamEvent;
use everevo_core::types::ToolCall;
use everevo_core::EverEvoError;
use tokio::sync::mpsc;

use super::event::AgentEvent;

/// Accumulated result of one token stream — what the loop needs after the
/// stream closes or hits Done.
pub(crate) struct StreamAccum {
    pub(crate) current_text: String,
    pub(crate) current_thinking: String,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) saw_server_tool: bool,
    pub(crate) last_stop_reason: Option<String>,
}

/// Drain `token_rx` into accumulated text / thinking / tool calls, forwarding
/// each event to `tx`. Stops on `Done` (drains pending tool args into
/// `tool_calls`) or channel close. Returns Err (after sending an Error event)
/// on a 120s stall or a provider stream error.
pub(crate) async fn process_token_stream(
    token_rx: &mut mpsc::Receiver<StreamEvent>,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<StreamAccum, EverEvoError> {
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut pending_tool: Vec<(String, String, String)> = Vec::new();
    let mut saw_server_tool = false;
    let mut last_stop_reason: Option<String> = None;

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
                // llama-server re-sends the id only once per tool call;
                // dedup so a repeated id doesn't open a fresh slot.
                if !pending_tool.iter().any(|(pid, _, _)| pid == &id) {
                    pending_tool.push((id, name, String::new()));
                }
            }
            StreamEvent::ToolCallArg { id, arg_delta } => {
                // llama-server (OpenAI stream) omits `id` on a tool call's
                // continuation chunks — an empty id belongs to the current
                // call, not a new one. Without this every delta after the
                // first `{` was dropped and the args never parsed as JSON.
                let target = if id.is_empty() {
                    pending_tool.last_mut()
                } else {
                    pending_tool.iter_mut().rev().find(|(pid, _, _)| pid == &id)
                };
                if let Some((_, _, args)) = target {
                    args.push_str(&arg_delta);
                }
            }
            StreamEvent::ServerToolUse { .. } => {
                // Provider-executed tool (native web search) — the provider
                // runs it within this turn; nothing to dispatch.
                saw_server_tool = true;
            }
            StreamEvent::Done { stop_reason, .. } => {
                last_stop_reason = stop_reason;
                for (id, name, args_str) in pending_tool.drain(..) {
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

    Ok(StreamAccum {
        current_text,
        current_thinking,
        tool_calls,
        saw_server_tool,
        last_stop_reason,
    })
}
