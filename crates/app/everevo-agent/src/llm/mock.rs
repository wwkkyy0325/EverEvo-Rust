//! Mock LLM provider — for deterministic testing.
//!
//! Two usage modes:
//! - **Declarative script** ([`MockScript`] / [`MockStep`]): a router that
//!   answers each LLM call in order — the "mock pipeline". Each step is exactly
//!   one LLM call; tool-call steps auto-generate the stream (ids, id-less
//!   deltas) so tests never hand-assemble `StreamEvent` sequences.
//! - **Legacy queues** (`with_text` / `with_stream` / `with_tool_call`): kept
//!   for backward compatibility with existing tests. The script takes
//!   precedence when both are configured.

use async_trait::async_trait;

use everevo_core::llm::{
    FinishReason, LlmMessage, LlmProvider, LlmResponse, StreamEvent, ToolSchema,
};
use everevo_core::EverEvoError;

/// One answer in a mock-LLM script. Each step answers EXACTLY one LLM call.
#[derive(Debug, Clone)]
pub enum MockStep {
    /// Answer a turn with plain text (no tools).
    Text(String),
    /// Answer a turn by calling these tools; the loop executes them and the
    /// NEXT step answers the tool-result turn. The tool-call stream (with
    /// sequential `call_N` ids) is generated automatically.
    Calls(Vec<(&'static str, serde_json::Value)>),
    /// Like [`MockStep::Calls`], but the tool-call arguments stream in the
    /// llama-server id-less style: the first chunk carries the id + a lone
    /// `{`, continuation chunks carry bare deltas with an empty id. Exercises
    /// the arg-delta dedup path in the loop.
    CallsIdless(Vec<(&'static str, String)>),
    /// A raw `StreamEvent` sequence for edge cases (thinking-only, native
    /// truncation, provider error events).
    Stream(Vec<StreamEvent>),
    /// Simulate an LLM error on this call.
    Err(String),
}

/// A declarative mock-LLM script — the "mock pipeline". Routes each LLM call
/// to the next step and tracks call ids so tool-call streams are deterministic.
#[derive(Debug, Clone, Default)]
pub struct MockScript {
    steps: Vec<MockStep>,
    next_id: usize,
}

impl MockScript {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            next_id: 1,
        }
    }

    /// Append a step. Builder-style, chainable.
    pub fn then(mut self, step: MockStep) -> Self {
        self.steps.push(step);
        self
    }

    fn pop(&mut self) -> Option<MockStep> {
        if self.steps.is_empty() {
            None
        } else {
            Some(self.steps.remove(0))
        }
    }

    fn alloc_ids(&mut self, n: usize) -> Vec<String> {
        let ids: Vec<String> = (0..n)
            .map(|i| format!("call_{}", self.next_id + i))
            .collect();
        self.next_id += n;
        ids
    }
}

/// Render a script step as a streaming response (used by `chat_stream`).
fn render_stream(
    step: &MockStep,
    script: &mut MockScript,
) -> Result<Vec<StreamEvent>, EverEvoError> {
    match step {
        MockStep::Text(t) => Ok(vec![
            StreamEvent::Text(t.clone()),
            StreamEvent::Done {
                input_tokens: 0,
                output_tokens: 0,
                stop_reason: Some("end_turn".into()),
            },
        ]),
        MockStep::Calls(calls) => {
            let mut evs = Vec::new();
            for ((name, args), id) in calls.iter().zip(script.alloc_ids(calls.len())) {
                evs.push(StreamEvent::ToolCallStart {
                    id: id.clone(),
                    name: (*name).into(),
                });
                evs.push(StreamEvent::ToolCallArg {
                    id,
                    arg_delta: serde_json::to_string(args).unwrap_or_default(),
                });
            }
            evs.push(StreamEvent::Done {
                input_tokens: 0,
                output_tokens: 0,
                stop_reason: Some("tool_calls".into()),
            });
            Ok(evs)
        }
        MockStep::CallsIdless(calls) => {
            let mut evs = Vec::new();
            for ((name, arg_json), id) in calls.iter().zip(script.alloc_ids(calls.len())) {
                evs.push(StreamEvent::ToolCallStart {
                    id: id.clone(),
                    name: (*name).into(),
                });
                // First chunk: id + a lone `{` (llama-server style).
                evs.push(StreamEvent::ToolCallArg {
                    id,
                    arg_delta: "{".into(),
                });
                // Continuation chunks: empty id, bare deltas. The accumulated
                // args must re-parse to the full JSON object.
                let body = arg_json
                    .strip_prefix('{')
                    .unwrap_or(arg_json)
                    .strip_suffix('}')
                    .unwrap_or(arg_json);
                evs.push(StreamEvent::ToolCallArg {
                    id: String::new(),
                    arg_delta: format!("{body}}}"),
                });
            }
            evs.push(StreamEvent::Done {
                input_tokens: 0,
                output_tokens: 0,
                stop_reason: Some("tool_calls".into()),
            });
            Ok(evs)
        }
        MockStep::Stream(events) => Ok(events.clone()),
        MockStep::Err(msg) => Err(EverEvoError::LlmProvider(msg.clone())),
    }
}

/// Render a script step as a plain `chat` response (used by `chat`).
fn render_chat(step: &MockStep) -> Result<LlmResponse, EverEvoError> {
    match step {
        MockStep::Text(t) => Ok(LlmResponse {
            content: Some(t.clone()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
        }),
        MockStep::Calls(calls) => Ok(LlmResponse {
            content: None,
            tool_calls: calls
                .iter()
                .enumerate()
                .map(|(i, (name, args))| everevo_core::types::ToolCall {
                    id: format!("call_chat_{i}"),
                    name: (*name).to_string(),
                    arguments: args.clone(),
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
        }),
        MockStep::CallsIdless(calls) => render_chat(&MockStep::Calls(
            calls
                .iter()
                .map(|(name, json)| (*name, serde_json::from_str(json).unwrap_or_default()))
                .collect(),
        )),
        MockStep::Stream(_) => Err(EverEvoError::LlmProvider(
            "MockStep::Stream used via chat(); use stream_chat".into(),
        )),
        MockStep::Err(msg) => Err(EverEvoError::LlmProvider(msg.clone())),
    }
}

/// Mock LLM provider for deterministic testing.
pub struct MockLlmProvider {
    responses: tokio::sync::Mutex<Vec<LlmResponse>>,
    stream_events: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    call_log: tokio::sync::Mutex<Vec<Vec<LlmMessage>>>,
    script: tokio::sync::Mutex<MockScript>,
}

impl MockLlmProvider {
    pub fn new() -> Self {
        Self {
            responses: tokio::sync::Mutex::new(Vec::new()),
            stream_events: tokio::sync::Mutex::new(Vec::new()),
            call_log: tokio::sync::Mutex::new(Vec::new()),
            script: tokio::sync::Mutex::new(MockScript::new()),
        }
    }

    /// Build a provider driven by a declarative [`MockScript`] — the "mock
    /// pipeline". Each script step answers one LLM call in order.
    pub fn from_script(script: MockScript) -> Self {
        Self {
            responses: tokio::sync::Mutex::new(Vec::new()),
            stream_events: tokio::sync::Mutex::new(Vec::new()),
            call_log: tokio::sync::Mutex::new(Vec::new()),
            script: tokio::sync::Mutex::new(script),
        }
    }

    // ── Legacy queues (backward compat; script takes precedence) ──────────

    pub fn with_text(self, text: impl Into<String>) -> Self {
        self.responses.try_lock().unwrap().push(LlmResponse {
            content: Some(text.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
        });
        self
    }

    pub fn with_tool_call(self, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        self.responses.try_lock().unwrap().push(LlmResponse {
            content: None,
            tool_calls: vec![everevo_core::types::ToolCall {
                id: format!("call_{}", uuid::Uuid::new_v4()),
                name: name.into(),
                arguments,
            }],
            finish_reason: FinishReason::ToolCalls,
        });
        self
    }

    pub fn with_response(self, resp: LlmResponse) -> Self {
        self.responses.try_lock().unwrap().push(resp);
        self
    }

    /// Queue a full streaming response as a sequence of `StreamEvent`s. When
    /// `chat_stream` is called it consumes one such sequence before falling
    /// back to `chat`.
    pub fn with_stream(self, events: Vec<StreamEvent>) -> Self {
        self.stream_events.try_lock().unwrap().push(events);
        self
    }

    // ── Introspection + assertions ─────────────────────────────────────────

    pub fn call_log(&self) -> Vec<Vec<LlmMessage>> {
        self.call_log.try_lock().unwrap().clone()
    }

    pub fn call_count(&self) -> usize {
        self.call_log.try_lock().unwrap().len()
    }

    /// Assert the agent made exactly `n` LLM calls.
    pub fn assert_call_count(&self, n: usize) {
        let count = self.call_count();
        assert_eq!(count, n, "expected {n} LLM calls, got {count}");
    }

    /// Assert that across all calls, the agent invoked `tool` with `args`.
    pub fn assert_calls_contain(&self, tool: &str, args: &serde_json::Value) {
        let log = self.call_log();
        for msgs in &log {
            for m in msgs {
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        if c.name == tool && &c.arguments == args {
                            return;
                        }
                    }
                }
            }
        }
        panic!("agent never called `{tool}` with `{args}` — full call log:\n{log:?}");
    }
}

impl Default for MockLlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn chat(
        &self,
        messages: &[LlmMessage],
        _tools: &[ToolSchema],
    ) -> Result<LlmResponse, EverEvoError> {
        self.call_log.lock().await.push(messages.to_vec());
        // Script route first (each step = exactly one call).
        {
            let mut s = self.script.lock().await;
            if let Some(step) = s.pop() {
                return render_chat(&step);
            }
        }
        // Legacy fallback.
        let resp = {
            let mut r = self.responses.lock().await;
            if r.is_empty() {
                None
            } else {
                Some(r.remove(0))
            }
        };
        resp.ok_or_else(|| EverEvoError::LlmProvider("Mock: no more responses".into()))
    }

    async fn chat_stream(
        &self,
        messages: &[LlmMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<StreamEvent>, EverEvoError> {
        self.call_log.lock().await.push(messages.to_vec());
        // Script route first.
        {
            let mut s = self.script.lock().await;
            if let Some(step) = s.pop() {
                return render_stream(&step, &mut s);
            }
        }
        // Legacy fallback.
        let e = {
            let mut se = self.stream_events.lock().await;
            if se.is_empty() {
                None
            } else {
                Some(se.remove(0))
            }
        };
        match e {
            Some(ev) => Ok(ev),
            None => {
                let r = self.chat(messages, &[]).await?;
                Ok(vec![StreamEvent::Text(r.content.unwrap_or_default())])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_basic() {
        let m = MockLlmProvider::new().with_text("hello");
        assert_eq!(
            m.chat(&[LlmMessage::user("hi")], &[])
                .await
                .unwrap()
                .content
                .unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn test_script_text_step_streams_then_ends() {
        let m = MockLlmProvider::from_script(
            MockScript::new().then(MockStep::Text("Final answer: 42".into())),
        );
        let evs = m
            .chat_stream(&[LlmMessage::user("solve")], &[])
            .await
            .unwrap();
        assert!(matches!(evs[0], StreamEvent::Text(_)));
        assert!(matches!(evs[1], StreamEvent::Done { .. }));
        m.assert_call_count(1);
    }

    #[tokio::test]
    async fn test_script_calls_roundtrip_then_text() {
        let m = MockLlmProvider::from_script(
            MockScript::new()
                .then(MockStep::Calls(vec![(
                    "echo",
                    serde_json::json!({"text": "hi"}),
                )]))
                .then(MockStep::Text("Final answer: hi".into())),
        );
        // Call 1: tool call stream.
        let evs = m
            .chat_stream(&[LlmMessage::user("echo hi")], &[])
            .await
            .unwrap();
        assert!(matches!(evs[0], StreamEvent::ToolCallStart { .. }));
        // Call 2: plain text.
        let evs = m
            .chat_stream(&[LlmMessage::user("echo hi")], &[])
            .await
            .unwrap();
        assert!(matches!(evs[0], StreamEvent::Text(_)));
        m.assert_call_count(2);
    }

    #[tokio::test]
    async fn test_script_idless_args_accumulate() {
        let m =
            MockLlmProvider::from_script(MockScript::new().then(MockStep::CallsIdless(vec![(
                "echo",
                r#"{"text":"hello"}"#.to_string(),
            )])));
        let evs = m
            .chat_stream(&[LlmMessage::user("echo hello")], &[])
            .await
            .unwrap();
        // id-less: first chunk carries id + `{`, second chunk empty-id.
        match (&evs[1], &evs[2]) {
            (
                StreamEvent::ToolCallArg { id, arg_delta },
                StreamEvent::ToolCallArg {
                    id: id2,
                    arg_delta: d2,
                },
            ) => {
                assert_eq!(id, "call_1");
                assert_eq!(arg_delta, "{");
                assert!(id2.is_empty(), "continuation chunks carry an empty id");
                assert_eq!(format!("{arg_delta}{d2}"), r#"{"text":"hello"}"#);
            }
            other => panic!("expected id-less arg chunks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_script_error_step() {
        let m = MockLlmProvider::from_script(MockScript::new().then(MockStep::Err("boom".into())));
        let err = m
            .chat_stream(&[LlmMessage::user("hi")], &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn test_assert_calls_contain_finds_agent_tool_usage() {
        // Simulate what the LOOP sends back: an assistant message carrying the
        // tool call, then the tool result. assert_calls_contain must find it.
        let m = MockLlmProvider::from_script(MockScript::new().then(MockStep::Text("ok".into())));
        let msgs = vec![LlmMessage {
            role: everevo_core::llm::LlmRole::Assistant,
            content: String::new(),
            thinking: None,
            tool_calls: Some(vec![everevo_core::types::ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "hello"}),
            }]),
            tool_call_id: None,
            images: Vec::new(),
        }];
        let _ = m.chat(&msgs, &[]).await;
        m.assert_calls_contain("echo", &serde_json::json!({"text": "hello"}));
    }
}
