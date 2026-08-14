//! AgentExecutor — the bridge between A2A protocol and EverEvo's AgentRun.
//!
//! ## Architecture
//!
//! ```text
//! A2A Message (Parts) → LlmMessage → AgentRun::run() → A2A Task (Artifacts)
//! ```
//!
//! The executor wraps the existing `AgentRun::run_to_string()` pattern
//! already used by workflow/scheduler, making A2A just another entry point
//! into the same agent infrastructure.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_agent::llm::HttpClient;
use everevo_core::llm::LlmMessage;
use everevo_core::tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use crate::error::A2aError;
use crate::types::{A2aMessage, A2aTask, Artifact, Part, TaskState, TaskStatus};

/// The executor handles converting A2A messages into LLM messages,
/// running the agent loop, and packaging the results back as A2A tasks.
#[async_trait]
pub trait A2aAgentExecutor: Send + Sync {
    /// Execute a task synchronously and return the completed task.
    async fn execute(
        &self,
        task_id: &str,
        context_id: &str,
        message: &A2aMessage,
        cancel: CancellationToken,
    ) -> Result<A2aTask, A2aError>;
}

/// Production executor — bridges to the real EverEvo AgentRun.
pub struct EverEvoExecutor {
    llm: Arc<HttpClient>,
    tools: Arc<ToolRegistry>,
    max_turns: usize,
}

impl EverEvoExecutor {
    pub fn new(llm: Arc<HttpClient>, tools: Arc<ToolRegistry>, max_turns: usize) -> Self {
        Self {
            llm,
            tools,
            max_turns,
        }
    }

    /// Convert A2A Parts → LlmMessages.
    fn to_llm_messages(message: &A2aMessage) -> Vec<LlmMessage> {
        let mut messages = Vec::new();
        // System instruction for A2A context
        messages.push(LlmMessage::system(
            "You are EverEvo, an AI agent responding to a request from another agent \
             via the A2A protocol. Respond naturally — your output will be returned \
             to the calling agent as an artifact.",
        ));

        // Convert parts
        for part in &message.parts {
            match part {
                Part::Text { text } => {
                    messages.push(match message.role.as_str() {
                        "user" => LlmMessage::user(text),
                        _ => LlmMessage::assistant(text),
                    });
                }
                Part::File {
                    name,
                    mime_type,
                    uri,
                    bytes: _,
                } => {
                    let desc = format!(
                        "[File: {} ({}) at {}]",
                        name.as_deref().unwrap_or("unnamed"),
                        mime_type.as_deref().unwrap_or("unknown"),
                        uri.as_deref().unwrap_or("inline")
                    );
                    messages.push(LlmMessage::user(&desc));
                }
                Part::Data { data, .. } => {
                    let text =
                        serde_json::to_string_pretty(data).unwrap_or_else(|_| format!("{data}"));
                    messages.push(LlmMessage::user(format!("[Structured data]:\n{text}")));
                }
            }
        }

        messages
    }

    /// Convert the final assistant response → A2A Artifacts + Task.
    fn to_a2a_task(task_id: &str, context_id: &str, result: &str, state: TaskState) -> A2aTask {
        let artifact = Artifact::new(vec![Part::text(result)], "everevo-response");

        let status_msg = A2aMessage::agent(vec![Part::text(result)]);

        A2aTask {
            id: task_id.into(),
            context_id: context_id.into(),
            status: TaskStatus::with_message(state, status_msg),
            artifacts: vec![artifact],
            history: None,
            metadata: None,
        }
    }
}

#[async_trait]
impl A2aAgentExecutor for EverEvoExecutor {
    async fn execute(
        &self,
        task_id: &str,
        context_id: &str,
        message: &A2aMessage,
        cancel: CancellationToken,
    ) -> Result<A2aTask, A2aError> {
        let llm_messages = Self::to_llm_messages(message);

        // Run the agent loop — same as workflow/scheduler sub-agent
        let llm: Arc<dyn everevo_core::LlmProvider> = self.llm.clone();
        let result = everevo_agent::AgentRun::sub_agent(self.max_turns)
            .run_to_string(llm, Arc::clone(&self.tools), llm_messages, cancel)
            .await;

        // Detect errors from the agent run. run_to_string() returns a plain string,
        // so we use a two-tier approach: explicit error markers (fast path) then
        // content-based heuristics (broad catch for unexpected failures).
        let is_error = result.is_empty()
            || result.starts_with("[AGENT_ERROR]")
            || result.starts_with("Error: ")
            || result.contains("[Cancelled]")
            || result.starts_with("Timeout")
            || result.starts_with("Authentication failed")
            || result.starts_with("Rate limited")
            || result.starts_with("Server error")
            || result.starts_with("Model overloaded")
            || result.starts_with("Connection failed")
            || result.starts_with("Network error")
            || result.starts_with("HTTP ")
            || result.starts_with("API error");
        let state = if is_error {
            TaskState::Failed
        } else {
            TaskState::Completed
        };

        Ok(Self::to_a2a_task(task_id, context_id, &result, state))
    }
}

/// Stub executor for testing — echoes the input.
pub struct EchoExecutor;

#[async_trait]
impl A2aAgentExecutor for EchoExecutor {
    async fn execute(
        &self,
        task_id: &str,
        context_id: &str,
        message: &A2aMessage,
        _cancel: CancellationToken,
    ) -> Result<A2aTask, A2aError> {
        let input = message.text_content().unwrap_or_default();
        Ok(EverEvoExecutor::to_a2a_task(
            task_id,
            context_id,
            &format!("Echo: {input}"),
            TaskState::Completed,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_llm_messages_user_text() {
        let msg = A2aMessage::user(vec![Part::text("hello world")]);
        let llm_msgs = EverEvoExecutor::to_llm_messages(&msg);
        // System instruction + 1 user message = 2
        assert_eq!(llm_msgs.len(), 2);
        assert_eq!(llm_msgs[1].content, "hello world");
    }

    #[test]
    fn test_to_llm_messages_with_file() {
        let msg = A2aMessage::user(vec![Part::file_uri(
            "doc.pdf",
            "application/pdf",
            "/doc.pdf",
        )]);
        let llm_msgs = EverEvoExecutor::to_llm_messages(&msg);
        assert_eq!(llm_msgs.len(), 2);
        assert!(llm_msgs[1].content.contains("doc.pdf"));
    }

    #[test]
    fn test_to_a2a_task_completed() {
        let task = EverEvoExecutor::to_a2a_task("t1", "c1", "result text", TaskState::Completed);
        assert_eq!(task.id, "t1");
        assert_eq!(task.status.state, TaskState::Completed);
        assert_eq!(task.artifacts.len(), 1);
    }

    #[test]
    fn test_echo_executor() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let msg = A2aMessage::user(vec![Part::text("hello")]);
        let executor = EchoExecutor;
        let task = rt
            .block_on(executor.execute("t1", "c1", &msg, CancellationToken::new()))
            .unwrap();
        assert_eq!(task.status.state, TaskState::Completed);
        let text = task.status.message.unwrap().text_content().unwrap();
        assert!(text.contains("Echo: hello"));
    }
}
