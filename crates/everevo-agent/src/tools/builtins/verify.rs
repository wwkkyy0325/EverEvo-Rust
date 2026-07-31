//! Verification agent tool — checks sub-agent results for correctness.
//!
//! Claude Code alignment: lightweight, no-LLM validation pass that catches
//! common failure patterns before the result reaches the user. For deep
//! semantic verification, spawn a reviewer sub-agent.
//!
//! ## Checks performed
//!
//! 1. **Empty result** — the most common sub-agent silent failure
//! 2. **Error markers** — "Error:", "FAILED", "[Cancelled]", "Timeout" etc.
//! 3. **Truncation markers** — "[truncated:" indicates incomplete output
//! 4. **Task keyword coverage** — expected terms from the task appear in result
//! 5. **Structure quality** — result has headings, code blocks, or lists
//! 6. **Length sanity** — too-short results for complex tasks are suspicious

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

pub struct VerifyTool;

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &str {
        "Verify"
    }

    fn description(&self) -> &str {
        "Verify the output of a previous task. Checks for correctness, \
         completeness, and edge cases. Use after sub-agent tasks complete \
         to ensure quality. Provide the task description and the result \
         to verify."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Description of the task to verify"
                },
                "result": {
                    "type": "string",
                    "description": "The result/content to verify"
                }
            },
            "required": ["task", "result"]
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
        let task = params["task"].as_str().unwrap_or("unknown task");
        let result = params["result"].as_str().unwrap_or("");

        let checks = run_verification_checks(task, result);

        let verdict = if checks.iter().any(|c| c.severity == "FAIL") {
            ("FAIL", "❌")
        } else if checks.iter().any(|c| c.severity == "WARN") {
            ("WARN", "⚠️")
        } else {
            ("PASS", "✅")
        };

        let check_lines: Vec<String> = checks
            .iter()
            .map(|c| format!("- {} **{}**: {}", c.icon, c.label, c.detail))
            .collect();

        Ok(ToolOutput {
            content: format!(
                "## Verification: {task}\n\n\
                 Verdict: {verdict_icon} **{verdict}**\n\n\
                 {checks}\n\n\
                 ---\n\
                 For deep semantic verification (logic bugs, security issues, \
                 edge cases), spawn a reviewer sub-agent with `task` tool.",
                verdict_icon = verdict.1,
                verdict = verdict.0,
                checks = check_lines.join("\n"),
            ),
            is_error: verdict.0 == "FAIL",
            ..Default::default()
        })
    }
}

// ── Checks ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Check {
    icon: &'static str,
    label: &'static str,
    detail: String,
    severity: &'static str, // PASS | WARN | FAIL
}

fn run_verification_checks(task: &str, result: &str) -> Vec<Check> {
    let mut checks = Vec::new();
    let result_lower = result.to_lowercase();
    let task_lower = task.to_lowercase();

    // ── 1. Empty result ──────────────────────────────────────────
    if result.is_empty() {
        checks.push(Check {
            icon: "❌",
            label: "Empty",
            detail: "Result is completely empty — likely a channel drop or LLM connection failure.".into(),
            severity: "FAIL",
        });
        return checks; // no point checking further
    } else {
        checks.push(Check {
            icon: "✅", label: "Non-empty", detail: format!("{} characters", result.len()), severity: "PASS",
        });
    }

    // ── 2. Error markers ─────────────────────────────────────────
    let error_patterns = [
        ("Error:", "LLM or tool error prefix"),
        ("FAILED", "Explicit FAILED marker"),
        ("[Cancelled]", "Cancellation marker"),
        ("Timeout", "Timeout marker"),
        ("Authentication failed", "API authentication failure"),
        ("Rate limited", "API rate limit"),
        ("Server error", "Server-side error"),
    ];
    let mut found_errors = Vec::new();
    for (pat, desc) in &error_patterns {
        if result_lower.contains(&pat.to_lowercase()) {
            found_errors.push(*desc);
        }
    }
    if found_errors.is_empty() {
        checks.push(Check {
            icon: "✅", label: "No errors", detail: "No error markers found in output".into(), severity: "PASS",
        });
    } else {
        checks.push(Check {
            icon: "❌", label: "Errors",
            detail: format!("Found: {}", found_errors.join(", ")),
            severity: "FAIL",
        });
    }

    // ── 3. Truncation ────────────────────────────────────────────
    if result.contains("[truncated:") {
        checks.push(Check {
            icon: "⚠️", label: "Truncated",
            detail: "Output was truncated — some content may be missing".into(),
            severity: "WARN",
        });
    } else {
        checks.push(Check {
            icon: "✅", label: "Complete", detail: "No truncation markers".into(), severity: "PASS",
        });
    }

    // ── 4. Keyword coverage ──────────────────────────────────────
    let task_keywords: Vec<&str> = task_lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() >= 3)
        .collect();
    let matched: Vec<&&str> = task_keywords.iter().filter(|kw| result_lower.contains(*kw)).collect();
    let coverage = if task_keywords.is_empty() { 100.0 } else {
        (matched.len() as f64 / task_keywords.len() as f64) * 100.0
    };
    if coverage >= 50.0 || task_keywords.len() <= 2 {
        checks.push(Check {
            icon: "✅", label: "Keywords",
            detail: format!("{}/{} task keywords found in result ({:.0}%)", matched.len(), task_keywords.len(), coverage),
            severity: "PASS",
        });
    } else {
        checks.push(Check {
            icon: "⚠️", label: "Keywords",
            detail: format!("Only {}/{} task keywords found ({:.0}%) — result may not address the task", matched.len(), task_keywords.len(), coverage),
            severity: "WARN",
        });
    }

    // ── 5. Structure ─────────────────────────────────────────────
    let has_headings = result.contains("##") || result.contains("# ");
    let has_code = result.contains("```");
    let has_list = result.contains("- ") || result.contains("* ") || result.contains("1. ");
    if has_headings || has_code || has_list {
        checks.push(Check {
            icon: "✅", label: "Structure",
            detail: format!("Contains: {}{}{}",
                if has_headings { "headings " } else { "" },
                if has_code { "code " } else { "" },
                if has_list { "lists" } else { "" }),
            severity: "PASS",
        });
    } else {
        checks.push(Check {
            icon: "⚠️", label: "Structure",
            detail: "No headings, code blocks, or lists — output may be unstructured".into(),
            severity: "WARN",
        });
    }

    // ── 6. Length sanity ─────────────────────────────────────────
    let task_len = task.len();
    if task_len > 100 && result.len() < 50 {
        checks.push(Check {
            icon: "⚠️", label: "Short",
            detail: format!("Task is {task_len} chars but result is only {} chars — suspiciously short", result.len()),
            severity: "WARN",
        });
    } else {
        checks.push(Check {
            icon: "✅", label: "Length", detail: format!("{} chars", result.len()), severity: "PASS",
        });
    }

    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_empty_result() {
        let checks = run_verification_checks("analyze code", "");
        assert!(checks.iter().any(|c| c.label == "Empty" && c.severity == "FAIL"));
    }

    #[test]
    fn test_verify_error_marker() {
        let checks = run_verification_checks("do X", "Error: connection refused");
        assert!(checks.iter().any(|c| c.label == "Errors" && c.severity == "FAIL"));
    }

    #[test]
    fn test_verify_good_result() {
        let result = "## Analysis\n\nFound 3 issues:\n- Issue 1: ...\n- Issue 2: ...\n\n```rust\nlet x = 42;\n```";
        let checks = run_verification_checks("analyze Rust code for issues", result);
        let has_fail = checks.iter().any(|c| c.severity == "FAIL");
        assert!(!has_fail, "Good result should not FAIL: {:?}", checks);
    }

    #[test]
    fn test_verify_truncated() {
        let checks = run_verification_checks("do X", "Some content [truncated: 5000 total chars]...");
        assert!(checks.iter().any(|c| c.label == "Truncated" && c.severity == "WARN"));
    }

    #[test]
    fn test_verify_keyword_mismatch() {
        let checks = run_verification_checks("implement authentication system with OAuth2", "ok done");
        assert!(checks.iter().any(|c| c.label == "Keywords" && c.severity == "WARN"));
    }
}
