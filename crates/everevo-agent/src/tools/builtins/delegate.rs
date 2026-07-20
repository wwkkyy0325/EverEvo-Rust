//! Task Tool — Claude Code pattern. Non-blocking: spawn, return immediately.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use uuid::Uuid;

use crate::orchestration::TaskType;
use crate::subagent_context::SubAgentContext;

pub struct TaskTool {
    sandbox_root: Arc<PathBuf>,
    base_tools: Arc<everevo_core::tool::ToolRegistry>,
    llm: Option<Arc<crate::llm::HttpClient>>,
    persona: Arc<std::sync::RwLock<Option<String>>>,
    result_tx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
    /// Pending sub-agent count — AgentLoop uses this to block Done
    /// while sub-agents are still running.
    pub pending: Arc<AtomicUsize>,
    /// Parent agent's work directory for path inheritance.
    parent_work_dir: Arc<std::sync::RwLock<Option<std::path::PathBuf>>>,
    /// Pre-built sub-agent context (populated by chat route from all pipelines).
    pub subagent_ctx: Arc<std::sync::RwLock<SubAgentContext>>,
}

impl TaskTool {
    pub fn new(sandbox_root: Arc<PathBuf>, base_tools: Arc<everevo_core::tool::ToolRegistry>, llm: Option<Arc<crate::llm::HttpClient>>) -> Self {
        Self {
            sandbox_root, base_tools, llm,
            persona: Arc::new(std::sync::RwLock::new(None)),
            result_tx: Arc::new(std::sync::Mutex::new(None)),
            pending: Arc::new(AtomicUsize::new(0)),
            parent_work_dir: Arc::new(std::sync::RwLock::new(None)),
            subagent_ctx: Arc::new(std::sync::RwLock::new(SubAgentContext::default())),
        }
    }
    /// Set the parent agent's work directory so sub-agents can access its files.
    pub fn set_parent_work_dir(&self, dir: std::path::PathBuf) {
        *self.parent_work_dir.write().unwrap() = Some(dir);
    }
    /// Get a receiver for the AgentLoop and store the sender.
    pub fn take_receiver(&self) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.result_tx.lock().unwrap() = Some(tx);
        rx
    }
    pub fn set_persona(&self, persona: String) { *self.persona.write().unwrap() = Some(persona); }

    fn dispatch_one(&self, desc: &str, stype: &str, _max_turns: usize) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let sandbox = Arc::clone(&self.sandbox_root);
        let tools = Arc::clone(&self.base_tools);
        let llm = self.llm.clone();
        let ctx = self.subagent_ctx.read().unwrap().clone();
        let tx = self.result_tx.lock().unwrap().clone();
        let pending = Arc::clone(&self.pending);
        let desc = desc.to_string();
        let stype = stype.to_string();
        tokio::spawn(async move {
            let Some(llm) = llm else {
                pending.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.map(|t| t.send("SubAgent failed: no LLM".into()));
                return;
            };
            let result = spawn_single(&sandbox, &tools, llm, &desc, &stype, &ctx).await;
            pending.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.map(|t| t.send(format!("[SubAgent ✅] {desc}\n\n{result}")));
        });
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }
    fn description(&self) -> &str {
        "Launch sub-agents. Use 'description' for single task, 'subtasks' array for PARALLEL. Sub-agents run in background — results appear when ready. The main agent continues while sub-agents work."
    }
    fn parameters_schema(&self) -> serde_json::Value { serde_json::json!({
        "type": "object", "properties": {
            "description": {"type": "string"},
            "subtasks": {"type": "array", "items": {"type": "object", "properties": {"description": {"type": "string"}, "subagent_type": {"type": "string"}}, "required": ["description"]}},
            "subagent_type": {"type": "string"},
            "max_turns": {"type": "integer", "description": "Max turns (0=unlimited)"}
        }
    })}
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, EverEvoError> {
        if let Some(subtasks) = params["subtasks"].as_array() {
            for st in subtasks {
                let d = st["description"].as_str().unwrap_or("unnamed");
                let t = st["subagent_type"].as_str().unwrap_or("code-explorer");
                self.dispatch_one(d, t, 0);
            }
            return Ok(ToolOutput { content: format!("{} subagents dispatched", subtasks.len()), is_error: false });
        }
        let desc = params["description"].as_str().or_else(|| params["task"].as_str()).unwrap_or("unnamed");
        let max_turns = params["max_turns"].as_u64().map(|v| v as usize).unwrap_or(0);
        let stype = params["subagent_type"].as_str().unwrap_or("code-explorer");
        self.dispatch_one(desc, stype, max_turns);
        Ok(ToolOutput { content: format!("SubAgent dispatched: {desc}"), is_error: false })
    }
}

async fn spawn_single(
    sandbox_root: &PathBuf,
    base_tools: &everevo_core::tool::ToolRegistry,
    llm: Arc<crate::llm::HttpClient>,
    desc: &str,
    stype: &str,
    sub_ctx: &SubAgentContext,
) -> String {
    let _task_type = match stype { "reviewer" => TaskType::ReviewTask, "research" => TaskType::ResearchTask, "file" => TaskType::FileOperation, _ => TaskType::CodeTask };

    let mut sub_tools = everevo_core::tool::ToolRegistry::new();
    if let Some(shell) = base_tools.get("shell") { sub_tools.register(Arc::clone(shell)); }
    if let Some(memory) = base_tools.get("memory") { sub_tools.register(Arc::clone(memory)); }

    // Build the full system prompt from the assembled context.
    let system_prompt = sub_ctx.build_system_prompt(desc);
    let messages = vec![
        everevo_core::llm::LlmMessage::system(&system_prompt),
        everevo_core::llm::LlmMessage::user(&format!(
            "Execute this task and return the result:\n\n{desc}\n\n\
             If you need to run shell commands, use the shell tool.\n\
             Report ALL findings including empty results.",
        )),
    ];

    // Run directly via AgentLoop — bypass SubAgent::execute() so we control
    // the system prompt and context assembly fully.
    let agent_loop = crate::AgentLoop::new(); // max_turns=0 = unlimited
    let mut rx = agent_loop.run(llm, Arc::new(sub_tools), messages, None).await;

    let mut final_text = String::new();
    let mut tool_call_count = 0usize;
    let start = std::time::Instant::now();
    let sa_id = Uuid::new_v4();

    while let Some(event) = rx.recv().await {
        match event {
            crate::AgentEvent::ToolCallStart { .. } => { tool_call_count += 1; }
            crate::AgentEvent::Done { final_text: text } => { final_text = text; break; }
            crate::AgentEvent::Error { message } => { final_text = message; break; }
            _ => {}
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    // Don't clean up — the sandbox dir is the sub-agent's workspace.
    // (The TaskTool is responsible for sandbox lifecycle if needed.)

    // Persist telemetry
    let persist_dir = sandbox_root.parent().unwrap_or(sandbox_root).join("telemetry").join("subagent_tasks");
    std::fs::create_dir_all(&persist_dir).ok();
    let _ = std::fs::write(persist_dir.join(format!("{}.json", sa_id)), serde_json::to_string_pretty(&serde_json::json!({
        "id": sa_id.to_string(), "task": desc, "success": true, "turns": 0, "duration_ms": duration_ms,
        "content": &final_text[..500.min(final_text.len())],
    })).unwrap_or_default());
    let is_error = final_text.is_empty();
    let meta = serde_json::json!({
        "agent_id": sa_id.to_string(),
        "task": desc,
        "status": if is_error { "FAILED" } else { "SUCCESS" },
        "turns": 0,
        "tool_calls": tool_call_count,
        "duration_ms": duration_ms,
        "timestamp": Utc::now().to_rfc3339(),
        "schema_version": "1.0",
    });
    format!("---SUBAGENT_RESULT---\n{}\n---END_RESULT---\n\n{}", serde_json::to_string_pretty(&meta).unwrap_or_default(), final_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_name() { assert_eq!(TaskTool::new(Arc::new(PathBuf::from("/tmp")), Arc::new(everevo_core::tool::ToolRegistry::new()), None).name(), "task"); }
    #[test]
    fn test_schema() { let t = TaskTool::new(Arc::new(PathBuf::from("/tmp")), Arc::new(everevo_core::tool::ToolRegistry::new()), None); assert!(t.parameters_schema()["properties"]["subtasks"].is_object()); }
    #[test]
    fn test_dispatch_returns_immediately() { let rt = tokio::runtime::Runtime::new().unwrap(); rt.block_on(async { let t = TaskTool::new(Arc::new(PathBuf::from("/tmp")), Arc::new(everevo_core::tool::ToolRegistry::new()), None); let r = t.execute(serde_json::json!({"description": "test"})).await.unwrap(); assert!(!r.is_error); assert!(r.content.contains("dispatched")); }); }
}
