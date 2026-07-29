//! Mock LLM provider — for testing.

use async_trait::async_trait;

use everevo_core::llm::{
    FinishReason, LlmMessage, LlmProvider, LlmResponse, StreamEvent, ToolSchema,
};
use everevo_core::EverEvoError;

/// Mock LLM provider for deterministic testing.
pub struct MockLlmProvider {
    responses: tokio::sync::Mutex<Vec<LlmResponse>>,
    stream_events: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
    call_log: tokio::sync::Mutex<Vec<Vec<LlmMessage>>>,
}

impl MockLlmProvider {
    pub fn new() -> Self {
        Self {
            responses: tokio::sync::Mutex::new(Vec::new()),
            stream_events: tokio::sync::Mutex::new(Vec::new()),
            call_log: tokio::sync::Mutex::new(Vec::new()),
        }
    }

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

    pub fn call_log(&self) -> Vec<Vec<LlmMessage>> {
        self.call_log.try_lock().unwrap().clone()
    }

    pub fn call_count(&self) -> usize {
        self.call_log.try_lock().unwrap().len()
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
        let e = {
            let mut s = self.stream_events.lock().await;
            if s.is_empty() {
                None
            } else {
                Some(s.remove(0))
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
}
