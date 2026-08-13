//! Content-block SSE state machine — converts AgentEvents to Anthropic content-block SSE events.
//!
//! Used by chat.rs to stream agent output. Encapsulates the block-index tracking,
//! thinking/text block open/close logic, and tool_use block emission.
//! Eliminates the 2× duplicated ~100-line streamer in handle_chat().

use super::stream;
use axum::response::sse::Event;
use everevo_agent::AgentEvent;
use std::convert::Infallible;
use tokio::sync::mpsc;

/// What action the caller should take after processing an event.
pub enum StreamerAction {
    /// Continue the event loop.
    Continue,
    /// Agent loop finished normally — caller should persist and close.
    Done,
    /// An error occurred — caller should report and close.
    Error { message: String },
}

/// Content-block state machine for SSE streaming.
///
/// Tracks which blocks are open (thinking, text) and emits Anthropic-compatible
/// content_block_start/delta/stop events in sequence.
pub struct ContentBlockStreamer {
    pub block_index: usize,
    pub thinking_open: bool,
    pub text_block_idx: Option<usize>,
    pub persisted_blocks: Vec<serde_json::Value>,
    pub full_response: String,
    /// Snapshot of accumulated thinking tokens.
    pub cur_thinking: String,
    /// Tool stubs awaiting results: (tool_id, tool_json, thinking_before_tool)
    pub pending_stubs: Vec<(String, serde_json::Value, Option<String>)>,
    /// Tool results collected: (tool_id, content, is_error)
    pub pending_results: Vec<(String, String, bool)>,
    /// Session ID for contextual SSE events (e.g., confirmation_required).
    pub session_id: uuid::Uuid,
}

impl ContentBlockStreamer {
    pub fn new(session_id: uuid::Uuid) -> Self {
        Self {
            block_index: 0,
            thinking_open: false,
            text_block_idx: None,
            persisted_blocks: Vec::new(),
            full_response: String::new(),
            cur_thinking: String::new(),
            pending_stubs: Vec::new(),
            pending_results: Vec::new(),
            session_id,
        }
    }

    /// Process one AgentEvent and emit corresponding SSE events.
    /// Returns the action the caller should take.
    pub async fn handle_event(
        &mut self,
        event: AgentEvent,
        tx: &mpsc::Sender<Result<Event, Infallible>>,
    ) -> StreamerAction {
        match event {
            AgentEvent::Thinking(t) => {
                self.cur_thinking.push_str(&t);
                if !self.thinking_open {
                    let _ = tx.send(stream::thinking_start(self.block_index)).await;
                    self.thinking_open = true;
                }
                let _ = tx.send(stream::thinking_delta(self.block_index, &t)).await;
                StreamerAction::Continue
            }
            AgentEvent::TextDelta(t) => {
                self.close_thinking(tx).await;
                if self.text_block_idx.is_none() {
                    self.text_block_idx = Some(self.block_index);
                    let _ = tx.send(stream::text_start(self.block_index)).await;
                }
                self.full_response.push_str(&t);
                let idx = self.text_block_idx.unwrap_or(self.block_index);
                let _ = tx.send(stream::text_delta(idx, &t)).await;
                StreamerAction::Continue
            }
            AgentEvent::ToolCallStart {
                id,
                name,
                arguments,
            } => {
                self.close_thinking(tx).await;
                self.close_text(tx).await;
                let tool_idx = self.block_index;
                self.block_index += 1;
                let thinking_snap = std::mem::take(&mut self.cur_thinking);
                let tool_json = serde_json::json!({"id": id, "name": name, "arguments": arguments});
                self.pending_stubs.push((
                    id.clone(),
                    tool_json,
                    if thinking_snap.is_empty() {
                        None
                    } else {
                        Some(thinking_snap)
                    },
                ));
                // Emit tool_use content block
                let _ = tx.send(stream::tool_start(tool_idx, &id, &name)).await;
                let args_str = serde_json::to_string(&arguments).unwrap_or_default();
                if !args_str.is_empty() && args_str != "null" {
                    let _ = tx.send(Ok(Event::default().event("content_block_delta").data(
                        serde_json::json!({"index": tool_idx, "delta": {"type": "input_json_delta", "partial_json": args_str}}).to_string(),
                    ))).await;
                }
                let _ = tx.send(stream::stop_event(tool_idx)).await;
                // Persist tool_use block
                self.persisted_blocks.push(serde_json::json!({
                    "index": tool_idx, "type": "tool_use",
                    "toolId": id.clone(), "toolName": name.clone(),
                    "toolInput": args_str,
                }));
                StreamerAction::Continue
            }
            AgentEvent::ToolCallEnd {
                id,
                name: _,
                content,
                is_error,
                // Images are carried in-memory to the LLM (vision); not persisted
                // to DB nor forwarded over SSE in this iteration.
                images: _,
            } => {
                if let Some(b) = self
                    .persisted_blocks
                    .iter_mut()
                    .rev()
                    .find(|b| b["type"] == "tool_use" && b["toolId"] == id)
                {
                    b["toolResult"] = serde_json::Value::String(content.clone());
                    b["toolError"] = serde_json::Value::Bool(is_error);
                }
                self.pending_results
                    .push((id.clone(), content.clone(), is_error));
                let _ = tx.send(Ok(Event::default().event("tool_result").data(
                    serde_json::json!({"tool_use_id": id, "content": content, "is_error": is_error}).to_string(),
                ))).await;
                StreamerAction::Continue
            }
            AgentEvent::ConfirmationNeeded { command, reason } => {
                let _ = tx
                    .send(Ok(Event::default().event("confirmation_required").data(
                        serde_json::json!({"session_id": self.session_id, "command": command, "reason": reason}).to_string(),
                    )))
                    .await;
                StreamerAction::Continue
            }
            AgentEvent::TurnComplete => StreamerAction::Continue,
            AgentEvent::Done { final_text } => {
                self.full_response = final_text;
                StreamerAction::Done
            }
            AgentEvent::Retrospective { summary } => {
                let _ = tx
                    .send(Ok(Event::default()
                        .event("retrospective")
                        .data(serde_json::json!({"summary": summary}).to_string())))
                    .await;
                StreamerAction::Continue
            }
            AgentEvent::Error { message } => {
                let _ = tx
                    .send(Ok(Event::default().event("error").data(&message)))
                    .await;
                StreamerAction::Error { message }
            }
            AgentEvent::WaitingForSubAgents { pending: p } => {
                let _ = tx
                    .send(Ok(Event::default()
                        .event("waiting")
                        .data(serde_json::json!({"pending": p}).to_string())))
                    .await;
                StreamerAction::Continue
            }
            AgentEvent::SubAgentResult {
                id,
                description,
                result,
            } => {
                let _ = tx.send(Ok(Event::default().event("subagent_result").data(
                    // Char-safe truncation — `&result[..N]` panics when a
                    // multi-byte UTF-8 char straddles byte N.
                    serde_json::json!({"id": id, "description": description, "result": result.chars().take(2000).collect::<String>()}).to_string(),
                ))).await;
                StreamerAction::Continue
            }
            AgentEvent::SubAgentStarted { id, description } => {
                let _ = tx
                    .send(Ok(Event::default().event("subagent_started").data(
                        serde_json::json!({"id": id, "description": description}).to_string(),
                    )))
                    .await;
                StreamerAction::Continue
            }
        }
    }

    /// Close the thinking block if open.
    async fn close_thinking(&mut self, tx: &mpsc::Sender<Result<Event, Infallible>>) {
        if self.thinking_open {
            let _ = tx.send(stream::stop_event(self.block_index)).await;
            self.persisted_blocks.push(serde_json::json!({
                "index": self.block_index, "type": "thinking", "thinking": self.cur_thinking.clone()
            }));
            self.block_index += 1;
            self.thinking_open = false;
        }
    }

    /// Close the text block if open.
    async fn close_text(&mut self, tx: &mpsc::Sender<Result<Event, Infallible>>) {
        if let Some(tb) = self.text_block_idx.take() {
            let _ = tx.send(stream::stop_event(tb)).await;
            self.persisted_blocks.push(serde_json::json!({
                "index": tb, "type": "text", "text": self.full_response.clone()
            }));
        }
    }
}

impl Default for ContentBlockStreamer {
    fn default() -> Self {
        Self::new(uuid::Uuid::nil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everevo_agent::AgentEvent;

    fn test_channel() -> (
        mpsc::Sender<Result<Event, Infallible>>,
        mpsc::Receiver<Result<Event, Infallible>>,
    ) {
        mpsc::channel::<Result<Event, Infallible>>(32)
    }

    #[tokio::test]
    async fn test_thinking_opens_block() {
        let mut s = ContentBlockStreamer::new(uuid::Uuid::nil());
        let (tx, mut rx) = test_channel();

        let action = s
            .handle_event(AgentEvent::Thinking("reasoning...".into()), &tx)
            .await;
        assert!(matches!(action, StreamerAction::Continue));
        assert!(s.thinking_open);
        assert_eq!(s.cur_thinking, "reasoning...");

        // Should have received at least one SSE event
        let first = rx.try_recv().is_ok();
        assert!(first, "should emit SSE events for thinking");
    }

    #[tokio::test]
    async fn test_done_sets_full_response() {
        let mut s = ContentBlockStreamer::new(uuid::Uuid::nil());
        let (tx, _rx) = test_channel();

        let action = s
            .handle_event(
                AgentEvent::Done {
                    final_text: "answer".into(),
                },
                &tx,
            )
            .await;
        assert!(matches!(action, StreamerAction::Done));
        assert_eq!(s.full_response, "answer");
    }

    #[tokio::test]
    async fn test_error_returns_action() {
        let mut s = ContentBlockStreamer::new(uuid::Uuid::nil());
        let (tx, _rx) = test_channel();

        let action = s
            .handle_event(
                AgentEvent::Error {
                    message: "fail".into(),
                },
                &tx,
            )
            .await;
        assert!(matches!(action, StreamerAction::Error { .. }));
    }

    #[tokio::test]
    async fn test_tool_call_updates_persisted_blocks() {
        let mut s = ContentBlockStreamer::new(uuid::Uuid::nil());
        let (tx, _rx) = test_channel();

        let _ = s
            .handle_event(AgentEvent::Thinking("think".into()), &tx)
            .await;
        let _ = s
            .handle_event(
                AgentEvent::ToolCallStart {
                    id: "tc1".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({"cmd": "ls"}),
                },
                &tx,
            )
            .await;
        let _ = s
            .handle_event(
                AgentEvent::ToolCallEnd {
                    id: "tc1".into(),
                    name: "shell".into(),
                    content: "file1\nfile2".into(),
                    is_error: false,
                    images: Vec::new(),
                },
                &tx,
            )
            .await;

        assert!(
            !s.persisted_blocks.is_empty(),
            "should have persisted blocks"
        );
        let tool_block = s
            .persisted_blocks
            .iter()
            .find(|b| b["type"] == "tool_use")
            .unwrap();
        assert_eq!(tool_block["toolId"], "tc1");
        assert_eq!(tool_block["toolResult"], "file1\nfile2");
        assert_eq!(tool_block["toolError"], false);
    }

    #[tokio::test]
    async fn test_block_index_increments() {
        let mut s = ContentBlockStreamer::new(uuid::Uuid::nil());
        let (tx, _rx) = test_channel();

        assert_eq!(s.block_index, 0);
        let _ = s.handle_event(AgentEvent::Thinking("t".into()), &tx).await;
        let _ = s
            .handle_event(AgentEvent::TextDelta("text".into()), &tx)
            .await;
        assert!(
            s.block_index >= 1,
            "block_index should increment after closing thinking"
        );
    }
}
