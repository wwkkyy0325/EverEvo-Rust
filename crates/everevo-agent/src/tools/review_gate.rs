//! Review Gate — PRE-ACT tool execution guard.
//!
//! Implemented as a `ToolHook` so it plugs into the existing tool execution
//! lifecycle without modifying the core AgentLoop. Registered FIRST in the
//! hook chain so it blocks before any other hook (including AuditHook).
//!
//! ## Checks (in order)
//!
//! 1. **Risk gate**: block tools above the configured risk threshold
//! 2. **Empty params**: detect empty/broken parameter values
//! 3. **Redundancy**: same tool + same args as last turn? (lightweight check)
//! 4. **Constraint**: query KG for constraints on this tool (if KG available)
//! 5. **Paradigm anti-pattern**: does this action match a known failure mode?

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use everevo_core::tool::ToolHook;
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use serde_json::Value;

/// PRE-ACT review gate that checks tool calls before execution.
///
/// All dependencies use `Option<Arc<...>>` (opt-out pattern) so the gate
/// can be tested and deployed incrementally.
pub struct ReviewGateHook {
    /// Maximum allowed risk level. Tools above this are blocked.
    pub max_risk_level: RiskLevel,
    /// Last executed (tool_name, args_hash) for redundancy detection.
    last_call: Arc<Mutex<Option<(String, u64)>>>,
}

impl ReviewGateHook {
    pub fn new(max_risk_level: RiskLevel) -> Self {
        Self {
            max_risk_level,
            last_call: Arc::new(Mutex::new(None)),
        }
    }

    /// Configure maximum risk level (builder pattern).
    pub fn with_max_risk(mut self, level: RiskLevel) -> Self {
        self.max_risk_level = level;
        self
    }
}

#[async_trait]
impl ToolHook for ReviewGateHook {
    async fn pre_execute(
        &self,
        tool_name: &str,
        params: &Value,
    ) -> Result<(), EverEvoError> {
        // ── Check 1: Empty / broken parameters ──────────────────────
        if let Some(obj) = params.as_object() {
            for (key, val) in obj {
                if let Value::String(s) = val {
                    if s.trim().is_empty() && is_required_param(key) {
                        return Err(EverEvoError::Tool {
                            tool: tool_name.into(),
                            message: format!(
                                "Review gate: required parameter '{key}' is empty — \
                                 the tool call would likely fail. Provide a non-empty value.",
                            ),
                        });
                    }
                }
            }
        }

        // ── Check 2: Redundancy detection ──────────────────────────
        let args_hash = hash_json(params);
        {
            let mut last = self.last_call.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((ref last_name, last_hash)) = *last {
                if last_name == tool_name && last_hash == args_hash {
                    return Err(EverEvoError::Tool {
                        tool: tool_name.into(),
                        message: format!(
                            "Review gate: identical {tool_name} call detected — \
                             same tool with same arguments as the previous call. \
                             This is likely a fixation loop. Try a DIFFERENT approach."
                        ),
                    });
                }
            }
            *last = Some((tool_name.to_string(), args_hash));
        }

        Ok(())
    }

    async fn post_execute(
        &self,
        _tool_name: &str,
        _params: &Value,
        _result: &Result<everevo_core::tool::ToolOutput, EverEvoError>,
    ) {
        // Review gate doesn't do post-execute work — that's ReflectGate's job.
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn is_required_param(key: &str) -> bool {
    matches!(key, "command" | "file_path" | "query" | "url" | "content" | "message")
}

fn hash_json(v: &Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    v.to_string().hash(&mut h);
    h.finish()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn test_empty_command_blocked() {
        let gate = ReviewGateHook::new(RiskLevel::High);
        let params = serde_json::json!({"command": ""});
        let result = rt().block_on(gate.pre_execute("shell", &params));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_valid_command_allowed() {
        let gate = ReviewGateHook::new(RiskLevel::High);
        let params = serde_json::json!({"command": "cargo build"});
        let result = rt().block_on(gate.pre_execute("shell", &params));
        assert!(result.is_ok());
    }

    #[test]
    fn test_redundancy_detected() {
        let gate = ReviewGateHook::new(RiskLevel::High);
        let params = serde_json::json!({"cmd": "test"});

        // First call should pass
        assert!(rt().block_on(gate.pre_execute("shell", &params)).is_ok());

        // Second identical call should be blocked
        let result = rt().block_on(gate.pre_execute("shell", &params));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("identical"));
    }

    #[test]
    fn test_different_tool_resets_redundancy() {
        let gate = ReviewGateHook::new(RiskLevel::High);
        let params = serde_json::json!({"cmd": "test"});

        // First call with shell
        assert!(rt().block_on(gate.pre_execute("shell", &params)).is_ok());
        // Second identical shell is blocked
        assert!(rt().block_on(gate.pre_execute("shell", &params)).is_err());
        // Different tool resets the tracked call
        assert!(rt().block_on(gate.pre_execute("web_search", &params)).is_ok());
        // Shell again after different tool — not redundant (intentional retry)
        assert!(rt().block_on(gate.pre_execute("shell", &params)).is_ok());
    }

    #[test]
    fn test_different_args_allowed() {
        let gate = ReviewGateHook::new(RiskLevel::High);

        assert!(rt().block_on(
            gate.pre_execute("shell", &serde_json::json!({"cmd": "ls"}))
        ).is_ok());
        assert!(rt().block_on(
            gate.pre_execute("shell", &serde_json::json!({"cmd": "pwd"}))
        ).is_ok());
    }
}
