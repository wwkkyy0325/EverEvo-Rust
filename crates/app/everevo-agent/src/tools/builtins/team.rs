//! TeamTool — multi-agent coordination with role-based sub-agents.
//!
//! Claude Code alignment: Agent Teams / Coordinator mode.
//! Dispatches N sub-agents in parallel, each with a role-specific
//! system prompt. The coordinator collects and synthesizes results.
//!
//! ## Roles
//!
//! - `reviewer` — Adversarial code review: bugs, security, test gaps
//! - `researcher` — Codebase investigation: patterns, architecture, findings
//! - `coder` — Implementation: make changes, verify, report
//! - `tester` — Test writing: add tests, run suite, report coverage
//! - `general` — General-purpose (default, no specialization)

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::delegate::{SubAgentHandle, SubAgentStatus};

// ── TeamRole ─────────────────────────────────────────────────────────────

/// Specialized agent role with a tailored system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TeamRole {
    Reviewer,
    Researcher,
    Coder,
    Tester,
    General,
}

impl TeamRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            TeamRole::Reviewer => "reviewer",
            TeamRole::Researcher => "researcher",
            TeamRole::Coder => "coder",
            TeamRole::Tester => "tester",
            TeamRole::General => "general",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "reviewer" => TeamRole::Reviewer,
            "researcher" => TeamRole::Researcher,
            "coder" => TeamRole::Coder,
            "tester" => TeamRole::Tester,
            _ => TeamRole::General,
        }
    }

    /// Role-specific system prompt injected as the FIRST message to the sub-agent.
    pub fn system_prompt(&self) -> &'static str {
        match self {
            TeamRole::Reviewer => "\
## Role: Code Reviewer

You are a critical, adversarial code reviewer. Your job is to find EVERY issue.

### What to look for:
1. **Correctness bugs** — logic errors, off-by-one, null/unwrap panics, race conditions
2. **Security vulnerabilities** — injection, path traversal, unsafe deserialization, missing auth
3. **Performance issues** — unnecessary allocations, blocking in async, O(n²) patterns
4. **Code quality** — missing error handling, unclear naming, dead code, missing tests
5. **Architecture violations** — cross-cutting dependencies, circular imports, layer violations

### Output format:
```
## Review: {task}

### Critical (must fix)
- [file:line] issue — why it matters — suggested fix

### Warnings (should fix)
- [file:line] issue — why it matters

### Suggestions (nice to have)
- suggestion
```
Be thorough. Find every issue. No false positives — verify each claim with file paths.",
            TeamRole::Researcher => "\
## Role: Researcher

You are a thorough codebase researcher. Your job is to investigate and document.

### What to do:
1. **Explore** — read files, follow imports, trace call chains
2. **Map** — identify patterns, architecture, module boundaries
3. **Document** — file paths, function signatures, data flow
4. **Connect** — find relationships between modules and subsystems

### Output format:
```
## Research: {task}

### Architecture
- Module structure, key types, dependencies

### Key Findings
- Discovery → file:line evidence

### Patterns
- Recurring patterns and conventions

### Recommendations
- Suggested approach based on findings
```
Leave no stone unturned. Every claim must have a file:line reference.",
            TeamRole::Coder => "\
## Role: Coder

You are a precise implementation engineer. Your job is to make changes correctly.

### What to do:
1. **Understand** — read the relevant code before changing anything
2. **Plan** — know exactly what files to change and why
3. **Implement** — make the minimal changes needed
4. **Verify** — run tests, check compilation, confirm no regressions

### Rules:
- Match existing code style (indentation, naming, patterns)
- Touch only what you must — don't refactor adjacent code
- Write tests for new behavior
- Run `cargo check` / `cargo test` after changes
- Report exactly what was changed and why
```
Report: files changed, lines added/removed, tests added, verification results.",
            TeamRole::Tester => "\
## Role: Tester

You are a test engineer. Your job is to ensure code quality through testing.

### What to do:
1. **Analyze** — identify untested code paths, edge cases, error conditions
2. **Write** — add unit tests, integration tests, edge case tests
3. **Run** — execute the test suite, verify all pass
4. **Report** — coverage gaps, flaky tests, missing assertions

### Test patterns:
- Happy path: normal inputs → expected outputs
- Edge cases: empty, null, max, min, boundary values
- Error paths: invalid inputs, timeouts, cancellations
- Regression: bugs that were fixed should have tests

### Output format:
```
## Test Report: {task}

### Tests Added
- test_name: what it verifies

### Test Results
- X passed, Y failed, Z ignored

### Coverage Gaps
- module::function — not tested because ...
```
Never weaken existing tests. If a test fails, the code is wrong — not the test.",
            TeamRole::General => "\
## Role: General Assistant

Complete the assigned task thoroughly and return a structured result with evidence (file paths, line numbers, test results).",
        }
    }

    /// Short description shown in the team dispatch UI.
    pub fn description(&self) -> &'static str {
        match self {
            TeamRole::Reviewer => "Critical code review — finds bugs, security issues, test gaps",
            TeamRole::Researcher => "Codebase investigation — maps architecture, finds patterns",
            TeamRole::Coder => "Implementation — writes code, makes changes, verifies",
            TeamRole::Tester => "Test engineering — adds tests, runs suite, reports coverage",
            TeamRole::General => "General-purpose task execution",
        }
    }
}

// ── TeamTool ─────────────────────────────────────────────────────────────

pub struct TeamTool {
    llm: Option<Arc<crate::llm::HttpClient>>,
    base_tools: Option<Arc<everevo_core::tool::ToolRegistry>>,
    sandbox_root: Option<Arc<std::path::PathBuf>>,
    handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
    statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
    /// Results from team members, keyed by subagent_id.
    results: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// Concurrency cap — prevents overwhelming the LLM API (default 8).
    max_concurrent: usize,
    /// Shared pending counter (from TaskTool) — auto-continue loop watches this.
    shared_pending: Option<super::delegate::SharedPending>,
    /// Shared results backlog (from TaskTool) — auto-continue loop drains this.
    shared_backlog: Option<super::delegate::SharedBacklog>,
    /// Result sender — feeds subagent_rx so the main loop injects [SubAgent Result].
    result_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl TeamTool {
    pub fn new() -> Self {
        Self {
            llm: None,
            base_tools: None,
            sandbox_root: None,
            handles: Arc::new(std::sync::Mutex::new(Vec::new())),
            statuses: Arc::new(std::sync::Mutex::new(Vec::new())),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            results: Arc::new(std::sync::Mutex::new(HashMap::new())),
            shared_pending: None,
            shared_backlog: None,
            result_tx: None,
            max_concurrent: 8,
        }
    }

    pub fn with_llm(mut self, llm: Arc<crate::llm::HttpClient>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_base_tools(mut self, tools: Arc<everevo_core::tool::ToolRegistry>) -> Self {
        self.base_tools = Some(tools);
        self
    }

    pub fn with_sandbox_root(mut self, root: Arc<std::path::PathBuf>) -> Self {
        self.sandbox_root = Some(root);
        self
    }

    /// Wire into the shared auto-continue system.
    /// Team member results are pushed to the shared backlog so the main
    /// agent loop can see completions and trigger auto-continue.
    pub fn with_shared_counters(
        mut self,
        pending: super::delegate::SharedPending,
        backlog: super::delegate::SharedBacklog,
    ) -> Self {
        self.shared_pending = Some(pending);
        self.shared_backlog = Some(backlog);
        self
    }

    /// Share the result notification channel so team member completions
    /// are injected into the main agent loop as `[SubAgent Result]` messages.
    /// Without this, team results only appear in the backlog and may be
    /// missed by the agent until the next auto-continue cycle.
    pub fn with_result_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self {
        self.result_tx = Some(tx);
        self
    }

    pub fn handles(&self) -> Arc<std::sync::Mutex<Vec<SubAgentHandle>>> {
        self.handles.clone()
    }

    pub fn statuses(&self) -> Arc<std::sync::Mutex<Vec<SubAgentStatus>>> {
        self.statuses.clone()
    }

    pub fn pending_count(&self) -> &Arc<std::sync::atomic::AtomicUsize> {
        &self.pending
    }

    /// Dispatch a team member sub-agent.
    /// Returns the sub-agent UUID for deterministic result collection.
    /// The semaphore permit is moved into the spawned task for concurrency control.
    fn dispatch_one(
        &self,
        description: &str,
        role: TeamRole,
        task_prompt: &str,
        max_turns: usize,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Uuid {
        let llm = match self.llm.clone() {
            Some(l) => l,
            None => {
                tracing::warn!("TeamTool: no LLM configured, skipping dispatch");
                return Uuid::nil();
            }
        };
        let tools = match self.base_tools.clone() {
            Some(t) => t,
            None => {
                tracing::warn!("TeamTool: no tools configured, skipping dispatch");
                return Uuid::nil();
            }
        };

        let subagent_id = Uuid::new_v4();
        let role_prompt = role.system_prompt();
        // Combine role prompt + task
        let full_prompt = format!("{role_prompt}\n\n## Task\n{task_prompt}",);

        let messages = vec![everevo_core::llm::LlmMessage::user(&full_prompt)];
        let max_turns = if max_turns == 0 { 3 } else { max_turns };
        let desc = format!("[{}] {}", role.as_str(), description);

        let handle = SubAgentHandle {
            id: subagent_id,
            description: desc.clone(),
            started_at: chrono::Utc::now(),
            cancel: CancellationToken::new(),
        };

        let status = SubAgentStatus {
            id: subagent_id,
            description: desc.clone(),
            status: "running".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: 0,
        };

        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(status);
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        // Also bump shared pending for auto-continue
        if let Some(ref sp) = self.shared_pending {
            sp.fetch_add(1, std::sync::atomic::Ordering::Release);
        }

        let statuses = self.statuses.clone();
        let pending = self.pending.clone();
        let results = self.results.clone();
        let shared_pending = self.shared_pending.clone();
        let shared_backlog = self.shared_backlog.clone();
        let result_tx = self.result_tx.clone();
        let subagent_id_str = subagent_id.to_string();
        let desc_clone = desc.clone();

        tokio::spawn(async move {
            let _permit = _permit; // hold semaphore permit until task completes
            let config = crate::loop_::AgentLoop::sub_agent(max_turns)
                .with_tool_result_budget(4000)
                .with_context_budget(40000);

            let result_text = config
                .run_subagent(llm, tools, messages, CancellationToken::new())
                .await;

            // Feed into subagent_rx so the main loop injects [SubAgent Result]
            if let Some(ref tx) = result_tx {
                let _ = tx.send(result_text.clone());
            }

            // Store result in team's own map
            results
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(subagent_id_str.clone(), result_text.clone());

            // Push into shared backlog so auto-continue loop can read it
            if let Some(ref bl) = shared_backlog {
                bl.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((subagent_id_str.clone(), desc_clone, result_text));
            }
            if let Some(ref sp) = shared_pending {
                sp.fetch_sub(1, std::sync::atomic::Ordering::Release);
            }

            // Update status
            if let Ok(mut s) = statuses.lock() {
                if let Some(st) = s.iter_mut().find(|st| st.id == subagent_id) {
                    st.status = "completed".into();
                }
            }
            pending.fetch_sub(1, std::sync::atomic::Ordering::Release);
        });

        subagent_id
    }
}

impl Default for TeamTool {
    fn default() -> Self {
        Self::new()
    }
}

use serde::{Deserialize, Serialize};

#[async_trait]
impl Tool for TeamTool {
    fn name(&self) -> &str {
        "team"
    }

    fn description(&self) -> &str {
        "Dispatch a team of role-specialized sub-agents to work on a task in parallel. \
         Each team member gets a role-specific system prompt (reviewer, researcher, coder, tester). \
         Results are collected and synthesized. Use for complex tasks that benefit from \
         multiple perspectives. \
         Parameters: task (the overall task), members (array of {{role, focus}} — what each member should focus on)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The overall task description shared by all team members"
                },
                "members": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": {
                                "type": "string",
                                "enum": ["reviewer", "researcher", "coder", "tester", "general"],
                                "description": "Role specialization"
                            },
                            "focus": {
                                "type": "string",
                                "description": "Specific focus area or sub-task for this member"
                            }
                        },
                        "required": ["role"]
                    }
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Max turns per member (default: 3)"
                }
            },
            "required": ["task", "members"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let task = params["task"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("task is required".into()))?;

        let members = params["members"]
            .as_array()
            .ok_or_else(|| EverEvoError::InvalidInput("members array is required".into()))?;

        let max_turns = params["max_turns"].as_u64().unwrap_or(3) as usize;

        if members.is_empty() {
            return Ok(ToolOutput {
                content: "No team members specified.".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let mut dispatched = Vec::new();
        let mut member_ids: Vec<(Uuid, String)> = Vec::new();
        // Semaphore for concurrency control (prevents overwhelming the LLM API)
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.max_concurrent.max(1)));
        for member in members {
            let role_str = member["role"].as_str().unwrap_or("general");
            let role = TeamRole::parse(role_str);
            let focus = member["focus"].as_str().unwrap_or(task);

            // Block until a permit is available (30s timeout to prevent hanging)
            let permit = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                semaphore.clone().acquire_owned(),
            )
            .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(_)) => {
                    tracing::error!("Team semaphore closed unexpectedly — aborting dispatch");
                    return Err(everevo_core::EverEvoError::Tool {
                        tool: "team".into(),
                        message: "Semaphore closed unexpectedly".into(),
                    });
                }
                Err(_elapsed) => {
                    tracing::error!("Team dispatch timed out waiting for slot (30s)");
                    return Err(everevo_core::EverEvoError::Tool {
                        tool: "team".into(),
                        message: "Timed out waiting for concurrent task slot".into(),
                    });
                }
            };
            let id = self.dispatch_one(focus, role, task, max_turns, permit);
            member_ids.push((
                id,
                format!("- **{}**: {}", role.as_str(), role.description()),
            ));
            let member_name = member_ids
                .last()
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            dispatched.push(member_name);
        }

        // Give sub-agents a brief window to start and return fast results.
        // No more 5-minute blocking — the main agent's auto-continue loop
        // will inject results via SSE as they complete.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Collect any results that arrived so far
        let results = self
            .results
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut synthesis = format!(
            "## Team Dispatch: {task}\n\n**{count} members dispatched:**\n{members}\n\n---\n\n",
            count = dispatched.len(),
            members = dispatched.join("\n"),
        );

        for (i, member) in members.iter().enumerate() {
            let role = TeamRole::parse(member["role"].as_str().unwrap_or("general"));
            let focus = member["focus"].as_str().unwrap_or(task);
            synthesis.push_str(&format!(
                "### Member {}: {} ({})\n\n",
                i + 1,
                role.as_str(),
                focus,
            ));

            // Deterministic result lookup by sub-agent UUID
            let result_text = member_ids
                .get(i)
                .and_then(|(id, _)| results.get(&id.to_string()))
                .cloned()
                .unwrap_or_else(|| {
                    if self.pending.load(std::sync::atomic::Ordering::Acquire) > 0 {
                        "(still running...)".into()
                    } else {
                        "(no result)".into()
                    }
                });

            // Truncate very long results
            let truncated = if result_text.chars().count() > 3000 {
                let safe: String = result_text.chars().take(3000).collect();
                format!("{safe}... (truncated)")
            } else {
                result_text
            };
            synthesis.push_str(&truncated);
            synthesis.push_str("\n\n---\n\n");
        }

        Ok(ToolOutput {
            content: synthesis,
            is_error: false,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_role_as_str() {
        assert_eq!(TeamRole::Reviewer.as_str(), "reviewer");
        assert_eq!(TeamRole::Coder.as_str(), "coder");
        assert_eq!(TeamRole::General.as_str(), "general");
    }

    #[test]
    fn test_team_role_from_str() {
        assert_eq!(TeamRole::parse("reviewer"), TeamRole::Reviewer);
        assert_eq!(TeamRole::parse("unknown"), TeamRole::General);
    }

    #[test]
    fn test_team_role_has_prompt() {
        for role in &[
            TeamRole::Reviewer,
            TeamRole::Researcher,
            TeamRole::Coder,
            TeamRole::Tester,
            TeamRole::General,
        ] {
            assert!(!role.system_prompt().is_empty());
            assert!(!role.description().is_empty());
        }
    }

    #[test]
    fn test_team_tool_name_and_schema() {
        let tool = TeamTool::new();
        assert_eq!(tool.name(), "team");
        assert_eq!(tool.risk_level(), RiskLevel::Medium);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "task");
        assert_eq!(schema["required"][1], "members");
    }
}
