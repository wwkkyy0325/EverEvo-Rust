//! TieredSandbox — resolves and delegates to the best available isolation tier.
//!
//! Every command passes through a single `check_permission()` chokepoint
//! before reaching the shell.

use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Instant;
use tokio::time::timeout;

use everevo_core::sandbox::{ExecutionConfig, ExecutionResult, SandboxProvider};
use everevo_core::EverEvoError;

use crate::audit::AuditRecord;
use crate::config::SandboxConfig;
use crate::permission::{check_permission, PermissionDecision, PermissionLevel, PermissionRules};
use crate::resolved::ShellResolver;

pub struct TieredSandbox {
    config: SandboxConfig,
    shell: crate::resolved::Shell,
    rules: std::sync::Mutex<PermissionRules>,
    audit_log: std::sync::Mutex<Vec<AuditRecord>>,
}

impl TieredSandbox {
    // ── Construction ─────────────────────────────────────────────────
    pub fn new(config: SandboxConfig) -> Result<Self, EverEvoError> {
        let shell = ShellResolver::resolve()
            .ok_or_else(|| EverEvoError::Sandbox("No shell available".into()))?;
        std::fs::create_dir_all(&config.sandbox_root).ok();
        Ok(Self {
            config,
            shell,
            rules: std::sync::Mutex::new(PermissionRules::default()),
            audit_log: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Poison-safe lock helper — recovers the inner data if a previous
    /// holder panicked while holding the lock.
    fn lock_rules(&self) -> std::sync::MutexGuard<'_, PermissionRules> {
        self.rules.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn with_permission_rules(self, rules: PermissionRules) -> Self {
        *self.lock_rules() = rules;
        self
    }

    pub fn set_permission_level(&self, level: PermissionLevel) {
        self.lock_rules().level = level;
    }

    pub fn permission_level(&self) -> PermissionLevel {
        self.lock_rules().level
    }

    /// Get a clone of the current permission rules.
    pub fn rules(&self) -> PermissionRules {
        self.lock_rules().clone()
    }

    /// Mutable access to rules (for trust escalation).
    pub fn rules_mut(&self) -> std::sync::MutexGuard<'_, PermissionRules> {
        self.lock_rules()
    }

    pub fn audit_log(&self) -> Vec<AuditRecord> {
        self.audit_log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn create_session_dir(&self, session_id: &str) -> Result<PathBuf, EverEvoError> {
        let dir = self.config.sandbox_root.join(session_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| EverEvoError::Sandbox(format!("session dir: {e}")))?;
        Ok(dir)
    }

    pub fn cleanup_session_dir(&self, session_id: &str) {
        let _ = std::fs::remove_dir_all(self.config.sandbox_root.join(session_id));
    }

    /// Check if a command is allowed under current rules WITHOUT executing it.
    ///
    /// Returns the decision so callers can present confirmation UI before
    /// calling `execute()`.
    pub fn check(&self, command: &str) -> PermissionDecision {
        check_permission(command, &self.lock_rules())
    }

    pub fn shell_name(&self) -> &str {
        &self.shell.name
    }

    // ── Command building ──────────────────────────────────────────────
    #[allow(clippy::disallowed_methods)]
    fn build_command(
        &self,
        ec: &ExecutionConfig,
        working_dir: &PathBuf,
    ) -> tokio::process::Command {
        let timeout_secs = ec.timeout_secs.min(self.config.max_timeout_secs);
        let memory_mb = ec
            .memory_limit_mb
            .or(self.config.default_memory_mb)
            .unwrap_or(512);

        let shell_args = self.shell_args(&ec.command, memory_mb, timeout_secs);
        let mut cmd = tokio::process::Command::new(&self.shell.executable);
        cmd.args(&shell_args)
            .current_dir(working_dir)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let mut path_parts: Vec<String> = self
            .config
            .injected_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        path_parts.push(std::env::var("PATH").unwrap_or_default());
        cmd.env(
            "PATH",
            path_parts.join(if cfg!(windows) { ";" } else { ":" }),
        );
        for (k, v) in &ec.env_vars {
            cmd.env(k, v);
        }
        for (k, v) in &self.config.injected_env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Build shell args for the current shell, wrapping WSL commands with
    /// Linux ulimit for resource isolation.
    fn shell_args(&self, command: &str, memory_mb: u64, timeout_secs: u64) -> Vec<String> {
        match self.shell.kind {
            crate::resolved::ShellKind::Wsl => {
                let mem_kb = memory_mb * 1024;
                let wrapped = format!(
                    "ulimit -v {mem_kb} -t {timeout_secs} -f {mem_kb} -u 64 2>/dev/null; {command}"
                );
                vec!["-e".into(), "sh".into(), "-c".into(), wrapped]
            }
            crate::resolved::ShellKind::GitBash => vec!["-c".into(), command.into()],
            crate::resolved::ShellKind::PowerShell => {
                vec!["-NoProfile".into(), "-Command".into(), command.into()]
            }
            crate::resolved::ShellKind::Cmd => vec!["/c".into(), command.into()],
            crate::resolved::ShellKind::Unix => {
                // On native Linux, use rlimit (set in unix_limits.rs) + command
                vec!["-c".into(), command.into()]
            }
        }
    }
}

// ── SandboxProvider trait impl ─────────────────────────────────────

#[async_trait]
impl SandboxProvider for TieredSandbox {
    async fn execute(&self, ec: &ExecutionConfig) -> Result<ExecutionResult, EverEvoError> {
        let start = Instant::now();
        let timeout_secs = ec.timeout_secs.min(self.config.max_timeout_secs);
        let working_dir = ec
            .working_dir
            .clone()
            .unwrap_or_else(|| self.config.sandbox_root.clone());

        // ── Chokepoint: Permission Gate ─────────────────────────────
        let decision = check_permission(&ec.command, &self.lock_rules());

        let (was_confirmed, final_decision) = match &decision {
            PermissionDecision::Deny { reason } => {
                tracing::warn!(command = %ec.command, %reason, "Sandbox blocked");
                let audit = AuditRecord {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    shell: self.shell.name.clone(),
                    command: redact_secrets(&ec.command),
                    working_dir: working_dir.display().to_string(),
                    timeout_secs,
                    exit_code: 126,
                    duration_ms: 0,
                    killed_by_timeout: false,
                    stdout_len: 0,
                    stderr_len: 0,
                    permission_level: self.lock_rules().level.label().into(),
                    was_confirmed: false,
                    requires_admin: false,
                    network_allowed: false,
                    memory_limit_mb: None,
                    job_object_applied: false,
                    external_paths: vec![],
                    decision: "deny".into(),
                };
                if let Ok(mut log) = self.audit_log.lock() {
                    log.push(audit);
                }
                return Ok(ExecutionResult {
                    stdout: String::new(),
                    stderr: format!("Blocked: {reason}"),
                    exit_code: 126,
                    duration_ms: 0,
                    killed_by_timeout: false,
                    needs_confirmation: false,
                    confirmation_reason: String::new(),
                });
            }
            PermissionDecision::Confirm {
                reason,
                external_paths: _ext_paths,
                requires_admin: _req_admin,
            } => {
                if !ec.confirmed {
                    // User has NOT explicitly confirmed — pause and ask.
                    // Return a result that tells the caller to present a
                    // confirmation dialog. The caller re-invokes execute()
                    // with confirmed: true after the user approves.
                    tracing::info!(
                        command = %ec.command,
                        %reason,
                        "Sandbox — awaiting user confirmation"
                    );
                    let audit = AuditRecord {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        shell: self.shell.name.clone(),
                        command: redact_secrets(&ec.command),
                        working_dir: working_dir.display().to_string(),
                        timeout_secs,
                        exit_code: 126,
                        duration_ms: 0,
                        killed_by_timeout: false,
                        stdout_len: 0,
                        stderr_len: 0,
                        permission_level: self.lock_rules().level.label().into(),
                        was_confirmed: false,
                        requires_admin: matches!(
                            &decision,
                            PermissionDecision::Confirm {
                                requires_admin: true,
                                ..
                            }
                        ),
                        network_allowed: ec.network_allowed,
                        memory_limit_mb: None,
                        job_object_applied: false,
                        external_paths: vec![],
                        decision: "confirm_pending".into(),
                    };
                    if let Ok(mut log) = self.audit_log.lock() {
                        log.push(audit);
                    }
                    return Ok(ExecutionResult {
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 0,
                        duration_ms: 0,
                        killed_by_timeout: false,
                        needs_confirmation: true,
                        confirmation_reason: reason.clone(),
                    });
                }
                // User confirmed — proceed with execution.
                tracing::info!(
                    command = %ec.command,
                    %reason,
                    "Sandbox exec — user confirmed"
                );
                (true, "confirmed".to_string())
            }
            PermissionDecision::Allow => {
                tracing::debug!(command = %ec.command, "Sandbox exec — auto-approved");
                (false, "allow".to_string())
            }
        };

        let audit_ts = chrono::Utc::now().to_rfc3339();
        tracing::info!(
            shell = %self.shell.name,
            command = %ec.command,
            timeout = timeout_secs,
            decision = %final_decision,
            "Sandbox exec"
        );

        // ── Shell Execution ─────────────────────────────────────────
        let mut cmd = self.build_command(ec, &working_dir);

        // Job Objects (Windows)
        #[cfg(windows)]
        let _job = if self.config.use_job_objects {
            match crate::job_object::JobObject::new() {
                Ok(j) => {
                    if let Some(mb) = ec.memory_limit_mb.or(self.config.default_memory_mb) {
                        let _ = j.set_memory_limit(mb as usize * 1024 * 1024);
                    }
                    Some(j)
                }
                Err(e) => {
                    tracing::warn!("JobObject failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        let result = timeout(std::time::Duration::from_secs(timeout_secs), cmd.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Extract external paths from the decision
        let ext_paths = match &decision {
            PermissionDecision::Confirm { external_paths, .. } => external_paths.clone(),
            _ => vec![],
        };

        let mut audit = AuditRecord {
            timestamp: audit_ts,
            shell: self.shell.name.clone(),
            command: redact_secrets(&ec.command),
            working_dir: working_dir.display().to_string(),
            timeout_secs,
            exit_code: -1,
            duration_ms,
            killed_by_timeout: false,
            stdout_len: 0,
            stderr_len: 0,
            permission_level: self.lock_rules().level.label().into(),
            was_confirmed,
            requires_admin: matches!(
                &decision,
                PermissionDecision::Confirm {
                    requires_admin: true,
                    ..
                }
            ),
            network_allowed: ec.network_allowed,
            memory_limit_mb: ec.memory_limit_mb.or(self.config.default_memory_mb),
            job_object_applied: self.config.use_job_objects,
            external_paths: ext_paths,
            decision: final_decision,
        };

        let output = match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                audit.exit_code = output.status.code().unwrap_or(-1);
                audit.stdout_len = stdout.len();
                audit.stderr_len = stderr.len();
                if audit.exit_code != 0 {
                    tracing::warn!(
                        exit_code = audit.exit_code,
                        duration_ms,
                        "Sandbox exec failed"
                    );
                }
                Ok(ExecutionResult {
                    stdout,
                    stderr,
                    exit_code: audit.exit_code,
                    duration_ms,
                    killed_by_timeout: false,
                    needs_confirmation: false,
                    confirmation_reason: String::new(),
                })
            }
            Ok(Err(e)) => Err(EverEvoError::Sandbox(format!("Process: {e}"))),
            Err(_) => {
                audit.killed_by_timeout = true;
                tracing::warn!(timeout = timeout_secs, "Timeout");
                Ok(ExecutionResult {
                    stdout: String::new(),
                    stderr: format!("Timeout after {timeout_secs}s"),
                    exit_code: -1,
                    duration_ms,
                    killed_by_timeout: true,
                    needs_confirmation: false,
                    confirmation_reason: String::new(),
                })
            }
        };

        if let Ok(mut log) = self.audit_log.lock() {
            log.push(audit);
        }
        output
    }
}

// ── Secret Redaction ─────────────────────────────────────────────────────

/// Redact API keys and secrets from shell commands before audit logging.
///
/// Masks patterns like:
/// - `Authorization: Bearer sk-xxx` / `x-api-key: xxx`
/// - `--header "Authorization: Bearer ..."`
/// - `ANTHROPIC_API_KEY=xxx` / `OPENAI_API_KEY=xxx`
/// - `-H "X-Api-Key: ..."`
fn redact_secrets(cmd: &str) -> String {
    use regex_lite::Regex;

    let mut result = cmd.to_string();

    // Bearer token in headers
    let re_bearer = Regex::new(r#"(?i)(bearer\s+)[\w\.\-_]+"#).unwrap();
    result = re_bearer.replace_all(&result, "${1}[REDACTED]").to_string();

    // x-api-key / api-key header values
    let re_apikey = Regex::new(r#"(?i)(x?-?api[_-]?key[\s:=]+)[\w\.\-_]+"#).unwrap();
    result = re_apikey.replace_all(&result, "${1}[REDACTED]").to_string();

    // Environment variable assignment with secret values
    let re_envkey = Regex::new(
        r#"(?i)((?:ANTHROPIC|OPENAI|DEEPSEEK|COHERE|HF)_?(?:API)?_?KEY\s*=\s*)[\w\.\-_]+"#,
    )
    .unwrap();
    result = re_envkey.replace_all(&result, "${1}[REDACTED]").to_string();

    // Authorization header in curl-style -H flags
    let re_curl_auth =
        Regex::new(r#"(?i)(-H\s*["']Authorization:\s*Bearer\s+)[\w\.\-_]+"#).unwrap();
    result = re_curl_auth
        .replace_all(&result, "${1}[REDACTED]")
        .to_string();

    // Generic secret-like patterns: key=xxxxxx (common in CLI tools)
    let re_keyval =
        Regex::new(r#"(?i)(--?(?:api[_-]?key|secret|token|password)\s+)[\w\.\-_/+=]+"#).unwrap();
    result = re_keyval.replace_all(&result, "${1}[REDACTED]").to_string();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_bearer() {
        let cmd = r#"curl -H "Authorization: Bearer sk-ant-api03-abc123def456" https://api.anthropic.com"#;
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-ant-api03-abc123def456"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn test_redact_api_key_header() {
        let cmd = r#"x-api-key: sk-proj-1234567890abcdef"#;
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-proj-1234567890abcdef"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn test_redact_env_var() {
        let cmd = r#"ANTHROPIC_API_KEY=sk-ant-xyz OPENAI_API_KEY=sk-proj-abc command"#;
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-ant-xyz"));
        assert!(!redacted.contains("sk-proj-abc"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
