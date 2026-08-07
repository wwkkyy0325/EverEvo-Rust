//! Integration test: Agent loop with MockLlmProvider.
//!
//! Demonstrates L2 testing — full ReAct loop, zero API cost.

use everevo_agent::llm::MockLlmProvider;
use everevo_core::llm::{FinishReason, LlmMessage, LlmProvider, LlmResponse, LlmRole};
use everevo_core::types::ToolCall;

/// Test: agent makes a tool call, receives the result, then gives a final answer.
#[tokio::test]
async fn test_agent_react_loop_mocked() {
    // ── Setup: configure a two-turn conversation ───────────────────
    let mock = MockLlmProvider::new()
        // Turn 1: LLM decides to call a tool
        .with_response(LlmResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_001".into(),
                name: "web_search".into(),
                arguments: serde_json::json!({"query": "Rust 2025 edition features"}),
            }],
            finish_reason: FinishReason::ToolCalls,
        })
        // Turn 2: LLM gives final answer (after receiving tool result)
        .with_text("The Rust 2025 edition introduces async closures and return-type notation.");

    // ── Simulate the agent loop ────────────────────────────────────
    let system = LlmMessage::system("You are a helpful assistant.");
    let user = LlmMessage::user("What's new in Rust 2025 edition?");

    // Turn 1: ask LLM
    let resp1 = mock
        .chat(&[system.clone(), user.clone()], &[])
        .await
        .unwrap();
    assert_eq!(resp1.tool_calls.len(), 1);
    assert_eq!(resp1.tool_calls[0].name, "web_search");

    // Agent executes tool (simulated)
    let tool_result =
        "Rust 2025 edition: async closures, return-type notation, RPIT lifetime capture...";

    // Turn 2: feed tool result back to LLM
    let tool_msg = LlmMessage::tool(tool_result, &resp1.tool_calls[0].id);
    let resp2 = mock.chat(&[system, user, tool_msg], &[]).await.unwrap();

    assert!(resp2.content.is_some());
    assert!(resp2.content.unwrap().contains("Rust 2025"));
    assert_eq!(mock.call_count(), 2);
}

/// Test: agent handles empty tool results gracefully.
#[tokio::test]
async fn test_agent_empty_tool_result() {
    let mock = MockLlmProvider::new()
        .with_tool_call("web_search", serde_json::json!({"query": "nonexistent"}))
        .with_text("I couldn't find any results for that query.");

    // Turn 1: LLM responds with a tool call
    let resp1 = mock
        .chat(&[LlmMessage::user("search for nonexistent")], &[])
        .await
        .unwrap();
    assert!(!resp1.tool_calls.is_empty());

    // Turn 2: tool returns empty, LLM gives final answer
    let resp2 = mock
        .chat(
            &[
                LlmMessage::user("search for nonexistent"),
                LlmMessage::tool("", "call_empty"),
            ],
            &[],
        )
        .await
        .unwrap();
    assert!(resp2.content.is_some());
}

/// Test: mock exhausted — tests that our error handling works.
#[tokio::test]
async fn test_mock_exhausted_errors_correctly() {
    let mock = MockLlmProvider::new().with_text("one and only");
    mock.chat(&[LlmMessage::user("hi")], &[]).await.unwrap();

    let err = mock
        .chat(&[LlmMessage::user("again")], &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no more responses"));
}

/// Test: verify call log captures entire conversation history.
#[tokio::test]
async fn test_call_log_captures_full_history() {
    let mock = MockLlmProvider::new()
        .with_tool_call("file_read", serde_json::json!({"path": "/tmp/x.txt"}))
        .with_text("File contents: hello world");

    // Turn 1
    mock.chat(&[LlmMessage::user("read /tmp/x.txt")], &[])
        .await
        .unwrap();

    // Turn 2: includes system + user + tool result
    mock.chat(
        &[
            LlmMessage::system("You are helpful."),
            LlmMessage::user("read /tmp/x.txt"),
            LlmMessage::tool("hello world", "call_001"),
        ],
        &[],
    )
    .await
    .unwrap();

    let log = mock.call_log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].len(), 1);
    assert_eq!(log[1].len(), 3);
    assert!(matches!(log[1][0].role, LlmRole::System));
}
