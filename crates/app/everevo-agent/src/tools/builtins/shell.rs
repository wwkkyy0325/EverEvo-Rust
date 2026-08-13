//! Shell execution tool — runs commands via the sandbox.
//!
//! Uses `Arc<dyn SandboxProvider>` for dependency injection — testable with mock sandbox.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::sandbox::{ExecutionConfig, SandboxProvider};
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

/// Execute shell commands via the sandbox.
pub struct ShellTool {
    sandbox: Arc<dyn SandboxProvider>,
}

impl ShellTool {
    pub fn new(sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in a sandboxed environment. \
         On Windows, uses Git Bash → PowerShell → CMD (WSL is opt-in). \
         Commands have a 30-second default timeout (max 300s) and run in an isolated workspace."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default: 30, max: 300)", "default": 30 },
                "working_dir": { "type": "string", "description": "Working directory (default: sandbox temp dir)" },
                "confirmed": { "type": "boolean", "description": "Set to true ONLY after the user has explicitly approved this exact command (e.g. a destructive git op or a ConfirmRequired gate). Bypasses the confirmation gate; the sandbox then executes without pausing." }
            },
            "required": ["command"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        // Check cancellation before spawning
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Ok(ToolOutput {
                content: "cancelled".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let command = params["command"]
            .as_str()
            .ok_or_else(|| EverEvoError::InvalidInput("command is required".into()))?;

        // Audit LOW (2026-08-13): `.min(300)` only capped the upper bound — a
        // `timeout_secs: 0` passed through, the sandbox fired a zero-duration
        // timeout that killed the child on first poll ("Timeout after 0s"), and
        // the one-shot compute retry (0*3) also died instantly. Floor at 1s.
        let mut timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30).clamp(1, 300);
        let working_dir = params["working_dir"].as_str().map(std::path::PathBuf::from);
        // Audit MEDIUM (2026-08-13): the gate text promised `confirmed: true`
        // but the parameter was never read — a re-invoke after user approval hit
        // the same gate forever (dead-end). Read it and plumb it into the
        // ExecutionConfig the sandbox already understands.
        let confirmed = params["confirmed"].as_bool().unwrap_or(false);

        let mut config = ExecutionConfig::new(command)
            .with_timeout(timeout_secs)
            .with_confirmed(confirmed);
        if let Some(dir) = working_dir {
            config = config.with_working_dir(dir);
        }

        // ── Git commit/push guard: always confirm destructive git operations ──
        // Bypassed only when the user has explicitly approved this exact command
        // (`confirmed: true` on the re-invoke after the confirmation prompt).
        if !confirmed && is_git_destructive(command) {
            return Ok(ToolOutput {
                content: format!(
                    "⚠️ 此 Git 操作需要你的确认。\n\n命令: {command}\n\
                     这将修改仓库历史或远程分支。\n\n\
                     请回复「确认执行」以继续，或「拒绝」以取消。\n\
                     (GitGuard: reply with \"确认执行\" to proceed)",
                ),
                is_error: false,
                ..Default::default()
            });
        }

        // Permission gate — check before execution
        let mut result = self.sandbox.execute(&config).await?;

        // ── Compute-timeout rescue (one-shot auto-retry) ─────────────
        // A compute cell (python/node/perl/awk…) that hit the default budget is
        // retried ONCE with a larger budget — the GAIA exact-DP pattern dies at
        // 30s and is recoverable. Gated to compute commands so interactive,
        // network, or side-effecting commands are never silently re-run.
        if result.killed_by_timeout && timeout_secs < 300 && is_compute_command(command) {
            let retry = (timeout_secs * 3).min(300);
            tracing::info!(
                command,
                timeout_secs,
                retry,
                "Compute cell timed out — one-shot retry"
            );
            let retry_config = config.clone().with_timeout(retry);
            result = self.sandbox.execute(&retry_config).await?;
            timeout_secs = retry;
        }

        // ── Confirmation gate ──────────────────────────────────────
        if result.needs_confirmation {
            let reason = &result.confirmation_reason;
            return Ok(ToolOutput {
                content: format!(
                    "⚠️ 此命令需要你的确认才能执行。\n\n命令: {command}\n原因: {reason}\n\n\
                     请回复「确认执行」以继续，或「拒绝」以取消。\n\
                     (ConfirmRequired: use `confirmed: true` to proceed)",
                ),
                is_error: false,
                ..Default::default()
            });
        }

        // Merge stdout and stderr — the LLM needs to see BOTH.
        // Many tools (cargo, npm, git) write diagnostics to stderr while
        // producing normal output on stdout; dropping stderr hides warnings.
        let mut content = String::new();
        if !result.stdout.is_empty() {
            content.push_str(&result.stdout);
        }
        if !result.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("--- stderr ---\n");
            content.push_str(&result.stderr);
        }

        // ── Exit code classification ──────────────────────────────
        if result.killed_by_timeout {
            return Ok(ToolOutput {
                content: format!(
                    "Timeout after {timeout_secs}s\n\n{content}\n\n\
                     If this is a long computation, re-run with `timeout_secs=300` \
                     and checkpoint partial state to `work/` between steps so \
                     progress survives the limit."
                ),
                is_error: true,
                ..Default::default()
            });
        }
        match result.exit_code {
            0 => Ok(ToolOutput {
                content,
                is_error: false,
                ..Default::default()
            }),
            126 => Ok(ToolOutput {
                // Permission denied by sandbox
                content: format!("Permission denied (exit 126)\n\n{content}"),
                is_error: true,
                ..Default::default()
            }),
            127 => Ok(ToolOutput {
                // Command not found
                content: format!("Command not found (exit 127)\n\n{content}"),
                is_error: true,
                ..Default::default()
            }),
            _ => Ok(ToolOutput {
                content: format!("Exit code {}\n\n{content}", result.exit_code),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

// ── Git Commit Guard ──────────────────────────────────────────────────────

/// Heuristic: is this command a pure compute cell (safe to auto-retry on
/// timeout)? Interpreter + an inline/script argument only — a bare interpreter
/// (REPL) or `-m <module>` (pip/venv, network/mutating) is excluded, as are
/// interactive, network, and file-mutating commands.
fn is_compute_command(command: &str) -> bool {
    let mut it = command.split_whitespace();
    let first = it.next().unwrap_or("");
    if !matches!(
        first,
        "python" | "python3" | "py" | "node" | "nodejs" | "perl" | "ruby" | "awk" | "bc" | "R"
    ) {
        return false;
    }
    // Bare interpreter with no args = interactive REPL — never auto-retry.
    let second = match it.next() {
        Some(s) => s,
        None => return false,
    };
    // `-m <module>` runs a module: pip/venv/etc. are network/mutating.
    if second == "-m" {
        return false;
    }
    true
}

/// Check if a command is a destructive git operation that should require
/// explicit user confirmation. Based on Claude Code community best practice:
/// deny `Bash(git commit:*)` and `Bash(git push:*)` by default.
fn is_git_destructive(command: &str) -> bool {
    let cmd = command.trim();
    if cmd.starts_with("git commit") {
        return true;
    }
    if cmd.starts_with("git push") {
        return true;
    }
    // git tag (creating, not listing)
    if cmd.starts_with("git tag") && !cmd.contains("-l") && !cmd.contains("--list") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records whether `execute` was reached.
    #[derive(Default)]
    struct CountingSandbox {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SandboxProvider for CountingSandbox {
        async fn execute(
            &self,
            _config: &ExecutionConfig,
        ) -> Result<everevo_core::sandbox::ExecutionResult, EverEvoError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(everevo_core::sandbox::ExecutionResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 0,
                killed_by_timeout: false,
                needs_confirmation: false,
                confirmation_reason: String::new(),
            })
        }
    }

    #[test]
    fn test_git_commit_flagged() {
        assert!(is_git_destructive("git commit -m 'test'"));
        assert!(is_git_destructive("git commit --amend"));
    }

    #[tokio::test]
    async fn test_destructive_git_requires_confirmed() {
        // Audit MEDIUM (2026-08-13): the gate promised `confirmed: true` but
        // never read it — the confirm prompt looped forever. Without the flag
        // the sandbox must NOT run; with it, the sandbox runs once.
        let calls = Arc::new(AtomicUsize::new(0));
        let sandbox: Arc<dyn SandboxProvider> = Arc::new(CountingSandbox {
            calls: Arc::clone(&calls),
        });
        let tool = ShellTool::new(sandbox);

        // Without confirmed: git guard fires, sandbox untouched.
        let r = tool
            .execute(serde_json::json!({"command": "git push origin main"}), None)
            .await
            .unwrap();
        assert!(r.content.contains("Git 操作需要你的确认"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // With confirmed:true the guard is bypassed and the sandbox runs.
        let r = tool
            .execute(
                serde_json::json!({"command": "git push origin main", "confirmed": true}),
                None,
            )
            .await
            .unwrap();
        // Not the confirmation gate — the sandbox returned an empty result.
        assert!(!r.content.contains("需要你的确认"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_git_push_flagged() {
        assert!(is_git_destructive("git push origin main"));
        assert!(is_git_destructive("git push --force"));
    }

    #[test]
    fn test_git_tag_flagged() {
        assert!(is_git_destructive("git tag v1.0"));
    }

    #[test]
    fn test_git_tag_list_not_flagged() {
        assert!(!is_git_destructive("git tag -l"));
    }

    #[test]
    fn test_git_status_not_flagged() {
        assert!(!is_git_destructive("git status"));
        assert!(!is_git_destructive("git log"));
        assert!(!is_git_destructive("git diff"));
        assert!(!is_git_destructive("git branch"));
    }

    #[test]
    fn test_non_git_not_flagged() {
        assert!(!is_git_destructive("cargo build"));
        assert!(!is_git_destructive("npm test"));
    }

    #[test]
    fn test_compute_commands_flagged() {
        assert!(is_compute_command("python -c 'print(1)'"));
        assert!(is_compute_command("python3 script.py"));
        assert!(is_compute_command("node solve.js"));
        assert!(is_compute_command(
            "awk '{sum+=$1} END{print sum}' data.txt"
        ));
        assert!(is_compute_command("perl -e 'print 1'"));
        assert!(is_compute_command("  python3 -c 'x=1'  ")); // leading/trailing ws
    }

    #[test]
    fn test_non_compute_commands_not_flagged() {
        assert!(!is_compute_command("git commit -m 'test'"));
        assert!(!is_compute_command("curl https://example.com"));
        assert!(!is_compute_command("ls -la"));
        assert!(!is_compute_command("python"));
        assert!(!is_compute_command(""));
        assert!(!is_compute_command("python3 -m pip install x 2>&1 | head"));
    }
}
