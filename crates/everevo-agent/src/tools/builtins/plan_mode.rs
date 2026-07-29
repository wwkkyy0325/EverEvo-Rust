//! Plan Mode tools — Claude Code-aligned state machine.
//!
//! ## Architecture
//!
//! Plan mode is a safety mechanism: when active, only read-only tools are
//! available. The agent explores, designs, writes a plan, and gets user
//! approval BEFORE any code is modified.
//!
//! ```
//! Normal → EnterPlanMode → PlanMode (write tools blocked, pre-perm saved)
//! PlanMode → ExitPlanMode  → Normal (permission restored, plan saved)
//!          → CancelPlanMode → Normal (permission restored, plan discarded)
//! ```
//!
//! ## Research basis
//!
//! - Claude Code: `/plan` command + LLM-initiated EnterPlanMode
//! - Cursor: Shift+Tab toggle, inline .plan.md editing
//! - Read-only tools during plan: Read, Grep, Glob, Agent (Claude Code exact set)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── Shared State ──────────────────────────────────────────────────────────

/// Per-session plan mode state, shared between tools and the chat route.
/// Key = session UUID, Value = pre-plan permission level (for restoration).
pub type PlanModeState = Arc<RwLock<HashMap<Uuid, String>>>;

/// Read-only tool names — allowed in plan mode (Claude Code alignment).
const READ_ONLY_TOOLS: &[&str] = &[
    "read_file", "list_dir", "code_search", "code_map",
    "memory", "web_fetch", "web_search",
    "EnterPlanMode", "ExitPlanMode",
    "Skill", "Verify",
    "TodoWrite", "Compact",
];

/// Check if a tool is allowed in plan mode.
pub fn is_tool_allowed_in_plan_mode(tool_name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&tool_name)
}

// ── EnterPlanMode ─────────────────────────────────────────────────────────

pub struct EnterPlanModeTool {
    plan_state: PlanModeState,
}

impl EnterPlanModeTool {
    pub fn new(plan_state: PlanModeState, _data_dir: PathBuf) -> Self {
        Self { plan_state }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Enter plan mode — a read-only exploration phase before implementation. \
         Use proactively when: (1) starting a non-trivial implementation task, \
         (2) multiple valid approaches exist, (3) code modifications will affect \
         existing behavior, (4) architectural decisions are needed, (5) the task \
         spans more than 2-3 files, (6) requirements are unclear, \
         (7) user preferences matter. \
         In plan mode you can: explore codebase, design approaches, write plans. \
         In plan mode you CANNOT: modify files, run shell commands, download files. \
         The user must approve your plan before you can implement."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let session_id = params["session_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil);

        let mut state = self.plan_state.write().await;

        // Already in plan mode — reject
        if state.contains_key(&session_id) {
            return Ok(ToolOutput {
                content: "Already in plan mode. Use ExitPlanMode to submit your plan \
                          or call CancelPlanMode to discard it.".into(),
                is_error: true,
            });
        }

        // Enter plan mode: store a placeholder (actual permission level set by chat route)
        state.insert(session_id, "semi_auto".to_string());

        tracing::info!(%session_id, "Entered plan mode");
        Ok(ToolOutput {
            content: "Plan mode entered. You are now in a read-only exploration phase.\n\n\
                      ## Plan Mode Workflow (5 Phases)\n\n\
                      1. **Understand** — Explore the codebase with code_search, code_map, \
                         read_file, list_dir. Launch Explore sub-agents for broad sweeps.\n\
                      2. **Design** — Design your approach. Consider trade-offs. \
                         If the design space is large, launch Plan sub-agents.\n\
                      3. **Review** — Ensure alignment with user intent. \
                         Ask clarifying questions if ANYTHING is ambiguous.\n\
                      4. **Write Plan** — Write a structured plan with: Context, Design, \
                         Implementation Steps, Files Changed, Verification.\n\
                      5. **ExitPlanMode** — Call ExitPlanMode with your plan summary. \
                         The plan will be saved and presented to the user for approval.\n\n\
                      ⚠️ Write tools (shell, write_file, download) are BLOCKED until \
                      the user approves your plan.".into(),
            is_error: false,
        })
    }
}

// ── ExitPlanMode ──────────────────────────────────────────────────────────

pub struct ExitPlanModeTool {
    plan_state: PlanModeState,
    data_dir: PathBuf,
}

impl ExitPlanModeTool {
    pub fn new(plan_state: PlanModeState, data_dir: PathBuf) -> Self {
        Self { plan_state, data_dir }
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Submit your plan for user approval and exit plan mode. \
         Provide a plan summary that explains what you will implement. \
         The plan will be saved to a file and the user will review it \
         before you can start implementing. \
         Parameters: plan (required — summary of your implementation plan)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "Summary of the plan for user review"
                }
            },
            "required": ["plan"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let session_id = params["session_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil);
        let plan_summary = params["plan"].as_str().unwrap_or("Plan ready for review");

        // Check if actually in plan mode
        {
            let state = self.plan_state.read().await;
            if !state.contains_key(&session_id) {
                return Ok(ToolOutput {
                    content: "Not currently in plan mode. Use EnterPlanMode first \
                              if you want to plan before implementing.".into(),
                    is_error: true,
                });
            }
        }

        // Save plan to file
        let plans_dir = self.data_dir.join("plans");
        let _ = std::fs::create_dir_all(&plans_dir);
        let slug = slugify(plan_summary);
        let plan_path = plans_dir.join(format!("{slug}.md"));
        let plan_content = format!(
            "# Plan\n\n{plan_summary}\n\n---\nSession: {session_id}\nStatus: pending_approval\n"
        );
        if let Err(e) = std::fs::write(&plan_path, &plan_content) {
            tracing::warn!(error = %e, path = %plan_path.display(), "Failed to save plan file");
        }

        // Clear plan mode state
        let old_perm = {
            let mut state = self.plan_state.write().await;
            state.remove(&session_id)
        };

        tracing::info!(%session_id, slug = %slug, "Exited plan mode, plan saved");

        Ok(ToolOutput {
            content: format!(
                "Plan submitted for approval.\n\n---\n{plan_summary}\n---\n\n\
                 Plan saved to: {plan_path}\n\
                 Previous permission level: {old_perm}\n\n\
                 ⏳ Waiting for user approval before implementation begins.\n\
                 DO NOT start implementing until the user explicitly approves.",
                plan_path = plan_path.display(),
                old_perm = old_perm.as_deref().unwrap_or("unknown"),
            ),
            is_error: false,
        })
    }
}

// ── Slug Generation ───────────────────────────────────────────────────────

/// Generate a readable slug from plan text.
/// Takes the first few meaningful words, lowercases, replaces spaces with dashes.
fn slugify(text: &str) -> String {
    let slug: String = text
        .chars()
        .take(80)
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect();
    let words: Vec<&str> = slug.split_whitespace().take(6).collect();
    if words.is_empty() {
        return "plan".to_string();
    }
    words.join("-").to_lowercase()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_simple() {
        let slug = slugify("Refactor the authentication system to use JWT");
        assert!(slug.contains("refactor"));
        assert!(slug.contains("authentication"));
        assert!(!slug.contains(" "));
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "plan");
    }

    #[test]
    fn test_is_tool_allowed() {
        assert!(is_tool_allowed_in_plan_mode("read_file"));
        assert!(is_tool_allowed_in_plan_mode("code_search"));
        assert!(!is_tool_allowed_in_plan_mode("shell"));
        assert!(!is_tool_allowed_in_plan_mode("write_file"));
    }

    #[test]
    fn test_enter_plan_mode_name_and_schema() {
        let state = Arc::new(RwLock::new(HashMap::new()));
        let tool = EnterPlanModeTool::new(state, PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "EnterPlanMode");
        assert_eq!(tool.risk_level(), RiskLevel::Low);
    }

    #[test]
    fn test_exit_plan_mode_not_in_plan() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let state = Arc::new(RwLock::new(HashMap::new()));
            let tool = ExitPlanModeTool::new(state, PathBuf::from("/tmp"));
            let result = tool.execute(
                serde_json::json!({"plan": "test plan"}),
                None,
            ).await.unwrap();
            assert!(result.is_error);
            assert!(result.content.contains("Not currently in plan mode"));
        });
    }

    #[test]
    fn test_enter_then_exit_plan_mode() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let session_id = Uuid::new_v4();
            let state: PlanModeState = Arc::new(RwLock::new(HashMap::new()));
            let enter = EnterPlanModeTool::new(Arc::clone(&state), PathBuf::from("/tmp"));
            let exit = ExitPlanModeTool::new(Arc::clone(&state), PathBuf::from("/tmp"));

            // Enter plan mode
            let r = enter.execute(
                serde_json::json!({"session_id": session_id.to_string()}),
                None,
            ).await.unwrap();
            assert!(!r.is_error);
            assert!(r.content.contains("Plan mode entered"));

            // Enter again should fail
            let r2 = enter.execute(
                serde_json::json!({"session_id": session_id.to_string()}),
                None,
            ).await.unwrap();
            assert!(r2.is_error);
            assert!(r2.content.contains("Already in plan mode"));

            // Exit plan mode
            let r3 = exit.execute(
                serde_json::json!({"session_id": session_id.to_string(), "plan": "Test plan"}),
                None,
            ).await.unwrap();
            assert!(!r3.is_error);
            assert!(r3.content.contains("Plan submitted"));

            // Should no longer be in plan mode
            assert!(state.read().await.get(&session_id).is_none());
        });
    }
}
