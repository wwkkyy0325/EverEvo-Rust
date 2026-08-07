//! Sub-agent spawn logic — builds the system prompt, runs the agent loop,
//! persists telemetry, and formats the result for injection into the main
//! conversation.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::subagent_context::SubAgentContext;

/// Type-specific guidance injected into the sub-agent system prompt.
pub(crate) fn stype_guidance(stype: &str) -> String {
    match stype {
        "reviewer" => "\n\n## Role: Code Reviewer\n\
            You are a critical code reviewer. Focus on:\n\
            - Correctness bugs and edge cases\n\
            - Security vulnerabilities\n\
            - Performance issues\n\
            - Adherence to project conventions\n\
            - Test coverage gaps\n\
            Be thorough and adversarial — find every issue.\n"
            .into(),
        "research" | "code-explorer" => "\n\n## Role: Researcher\n\
            You are a thorough researcher. Focus on:\n\
            - Exploring all relevant files and patterns\n\
            - Finding connections across modules\n\
            - Documenting your findings with file paths and line numbers\n\
            - Providing a structured, comprehensive report\n\
            Leave no stone unturned.\n"
            .into(),
        "file" => "\n\n## Role: File Operations\n\
            You are a precise file operator. Focus on:\n\
            - Making the requested file changes exactly as specified\n\
            - Verifying each change with tests or checks\n\
            - Leaving no unintended side effects\n\
            - Reporting what was changed and why.\n"
            .into(),
        _ => "\n\n## Role: General Assistant\n\
            Complete the assigned task thoroughly and return a structured result.\n"
            .into(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_single(
    sandbox_root: &Path,
    base_tools: &everevo_core::tool::ToolRegistry,
    llm: Arc<crate::llm::HttpClient>,
    desc: &str,
    stype: &str,
    sub_ctx: &SubAgentContext,
    max_turns: usize,
    cancel: CancellationToken,
) -> String {
    let mut child_ctx = sub_ctx.clone();
    child_ctx.depth = sub_ctx.depth.saturating_add(1);

    let mut sub_tools = everevo_core::tool::ToolRegistry::new();
    for name in base_tools.names() {
        if let Some(tool) = base_tools.get(name) {
            sub_tools.register(Arc::clone(tool));
        }
    }

    let mut system_prompt = child_ctx.build_system_prompt(desc);
    system_prompt.push_str(&stype_guidance(stype));

    let messages = vec![
        everevo_core::llm::LlmMessage::system(&system_prompt),
        everevo_core::llm::LlmMessage::user(format!(
            "Execute this task and return the result:\n\n{desc}\n\n\
             If you need to run shell commands, use the shell tool.\n\
             Report ALL findings including empty results.",
        )),
    ];

    let start = std::time::Instant::now();
    let sa_id = Uuid::new_v4();
    let agent_loop = crate::AgentLoop::sub_agent(max_turns);
    let final_text = agent_loop
        .run_subagent(llm, Arc::new(sub_tools), messages, cancel)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let persist_dir = sandbox_root
        .parent()
        .unwrap_or(sandbox_root)
        .join("telemetry")
        .join("subagent_tasks");
    std::fs::create_dir_all(&persist_dir).ok();
    let content_len = final_text.len();
    let meta_note = if content_len == 0 {
        "empty response — likely channel drop or LLM connection failure"
    } else if duration_ms < 3000 && final_text.starts_with("Error:") {
        "fast failure — likely LLM API error"
    } else {
        "sub-agent completed"
    };
    let _ = std::fs::write(
        persist_dir.join(format!("{}.json", sa_id)),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": sa_id.to_string(),
            "task": desc,
            "success": !final_text.is_empty() && !final_text.starts_with("Error:"),
            "duration_ms": duration_ms,
            "content_len": content_len,
            "note": meta_note,
            "content": &final_text[..500.min(final_text.len())],
        }))
        .unwrap_or_default(),
    );

    let is_error = final_text.is_empty()
        || final_text.starts_with("Error:")
        || final_text.contains("[Cancelled]")
        || final_text.starts_with("Timeout")
        || final_text.starts_with("Authentication failed")
        || final_text.starts_with("Rate limited")
        || final_text.starts_with("Server error")
        || final_text.starts_with("Model overloaded")
        || final_text.starts_with("Bad request")
        || final_text.starts_with("Connection failed")
        || final_text.starts_with("Network error")
        || final_text.starts_with("API error")
        || final_text.starts_with("Invalid request")
        || final_text.starts_with("Failed to read response");
    let meta = serde_json::json!({
        "agent_id": sa_id.to_string(),
        "task": desc,
        "status": if is_error { "FAILED" } else { "SUCCESS" },
        "duration_ms": duration_ms,
        "content_len": final_text.len(),
        "timestamp": Utc::now().to_rfc3339(),
        "schema_version": "1.0",
    });
    format!(
        "---SUBAGENT_RESULT---\n{}\n---END_RESULT---\n\n{}",
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
        final_text
    )
}
