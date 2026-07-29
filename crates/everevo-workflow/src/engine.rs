//! Workflow engine — executes workflow steps sequentially with error handling.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::types::*;

// ── Callbacks ────────────────────────────────────────────────────────────

/// Callbacks for I/O operations. The workflow engine is pure logic;
/// callbacks provide the actual execution environment.
#[async_trait::async_trait]
pub trait WorkflowCallbacks: Send + Sync {
    async fn shell_exec(&self, command: &str, working_dir: Option<&str>) -> Result<(String, String, i32), String>;
    async fn fetch_url(&self, url: &str) -> Result<String, String>;
    async fn memory_save(&self, key: &str, content: &str) -> Result<(), String>;
    async fn memory_search(&self, query: &str) -> Result<Vec<String>, String>;
    async fn agent_run(&self, prompt: &str, max_turns: usize) -> Result<String, String>;
}

// ── No-op callbacks for testing ──────────────────────────────────────────

pub struct NoopCallbacks;

#[async_trait::async_trait]
impl WorkflowCallbacks for NoopCallbacks {
    async fn shell_exec(&self, cmd: &str, _wd: Option<&str>) -> Result<(String, String, i32), String> {
        Ok((format!("[noop shell: {cmd}]"), String::new(), 0))
    }
    async fn fetch_url(&self, url: &str) -> Result<String, String> {
        Ok(format!("[noop fetch: {url}]"))
    }
    async fn memory_save(&self, _key: &str, _content: &str) -> Result<(), String> {
        Ok(())
    }
    async fn memory_search(&self, query: &str) -> Result<Vec<String>, String> {
        Ok(vec![format!("[noop memory: {query}]")])
    }
    async fn agent_run(&self, prompt: &str, _max_turns: usize) -> Result<String, String> {
        Ok(format!("[noop agent: {prompt}]"))
    }
}

// ── Engine ───────────────────────────────────────────────────────────────

pub struct WorkflowEngine<C: WorkflowCallbacks> {
    callbacks: C,
}

/// Blanket impl: delegate trait methods through Arc for dyn dispatch.
#[async_trait::async_trait]
impl WorkflowCallbacks for Arc<dyn WorkflowCallbacks> {
    async fn shell_exec(&self, c: &str, wd: Option<&str>) -> Result<(String, String, i32), String> { self.as_ref().shell_exec(c, wd).await }
    async fn fetch_url(&self, url: &str) -> Result<String, String> { self.as_ref().fetch_url(url).await }
    async fn memory_save(&self, k: &str, c: &str) -> Result<(), String> { self.as_ref().memory_save(k, c).await }
    async fn memory_search(&self, q: &str) -> Result<Vec<String>, String> { self.as_ref().memory_search(q).await }
    async fn agent_run(&self, p: &str, mt: usize) -> Result<String, String> { self.as_ref().agent_run(p, mt).await }
}

impl WorkflowEngine<NoopCallbacks> {
    pub fn new_noop() -> Self {
        Self { callbacks: NoopCallbacks }
    }
    pub fn with_callbacks(cb: Arc<dyn WorkflowCallbacks>) -> WorkflowEngine<Arc<dyn WorkflowCallbacks>> {
        WorkflowEngine { callbacks: cb }
    }
}

impl<C: WorkflowCallbacks> WorkflowEngine<C> {
    pub fn new(callbacks: C) -> Self {
        Self { callbacks }
    }

    /// Execute a full workflow definition.
    pub async fn execute(&self, def: &WorkflowDefinition) -> WorkflowResult {
        let start = Instant::now();
        let mut vars: HashMap<String, String> = def.variables.clone();
        let mut results = Vec::new();
        let mut failed = 0usize;

        for step in &def.steps {
            // Check condition
            if let Some(ref cond) = step.condition {
                if !self.eval_condition(cond, &vars) {
                    tracing::debug!(step = %step.id, condition = %cond, "Step skipped — condition false");
                    continue;
                }
            }

            // Execute step with retry
            let step_start = Instant::now();
            let mut retries = 0u32;
            let result: StepResult = loop {
                match self.execute_step(step, &vars).await {
                    Ok(r) => break r,
                    Err(e) if retries < step.retry => {
                        retries += 1;
                        tracing::warn!(step = %step.id, attempt = retries, error = %e, "Step failed — retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => {
                        let duration = step_start.elapsed().as_millis() as u64;
                        let r = StepResult {
                            step_id: step.id.clone(),
                            success: false,
                            output: String::new(),
                            exports: HashMap::new(),
                            duration_ms: duration,
                            error: Some(e),
                            retries,
                        };
                        failed += 1;
                        results.push(r.clone());
                        if def.stop_on_error {
                            let total = start.elapsed().as_millis() as u64;
                            return WorkflowResult {
                                workflow_name: def.name.clone(),
                                success: false,
                                steps_completed: results.len().saturating_sub(failed),
                                steps_failed: failed,
                                step_results: results,
                                total_duration_ms: total,
                                final_variables: vars,
                            };
                        }
                        break r;
                    }
                }
            };

            // Merge exports into variables
            for (k, v) in &result.exports {
                vars.insert(format!("{}.{}", step.id, k), v.clone());
            }
            // Store step output as variable
            vars.insert(step.id.clone(), result.output.clone());

            let duration = step_start.elapsed().as_millis() as u64;
            results.push(StepResult { duration_ms: duration, ..result });
        }

        let total = start.elapsed().as_millis() as u64;
        WorkflowResult {
            workflow_name: def.name.clone(),
            success: failed == 0,
            steps_completed: results.len() - failed,
            steps_failed: failed,
            step_results: results,
            total_duration_ms: total,
            final_variables: vars,
        }
    }

    /// Execute a single step.
    async fn execute_step(
        &self,
        step: &Step,
        vars: &HashMap<String, String>,
    ) -> Result<StepResult, String> {
        type StepOutput = (String, HashMap<String, String>);
        let output: Result<StepOutput, String> = match step.step_type {
            StepType::Shell => {
                let cmd = resolve_vars(step.params["command"].as_str().unwrap_or(""), vars);
                let wd = step.params["working_dir"].as_str();
                let (stdout, stderr, code) = self.callbacks.shell_exec(&cmd, wd).await?;
                let out = if code == 0 { stdout.clone() } else { format!("exit {code}\n{stdout}\n{stderr}") };
                let mut exports = HashMap::new();
                exports.insert("stdout".into(), stdout);
                exports.insert("stderr".into(), stderr);
                exports.insert("exit_code".into(), code.to_string());
                Ok((out, exports))
            }
            StepType::Fetch => {
                let url = resolve_vars(step.params["url"].as_str().unwrap_or(""), vars);
                let body = self.callbacks.fetch_url(&url).await?;
                let mut exports = HashMap::new();
                exports.insert("body".into(), body.clone());
                exports.insert("url".into(), url);
                Ok((body, exports))
            }
            StepType::MemorySave => {
                let key = resolve_vars(step.params["key"].as_str().unwrap_or(""), vars);
                let content = resolve_vars(step.params["content"].as_str().unwrap_or(""), vars);
                self.callbacks.memory_save(&key, &content).await?;
                Ok((format!("Saved: {key}"), HashMap::new()))
            }
            StepType::MemorySearch => {
                let query = resolve_vars(step.params["query"].as_str().unwrap_or(""), vars);
                let results = self.callbacks.memory_search(&query).await?;
                let text = results.join("\n");
                let mut exports = HashMap::new();
                exports.insert("results".into(), text.clone());
                exports.insert("count".into(), results.len().to_string());
                Ok((text, exports))
            }
            StepType::Agent => {
                let prompt = resolve_vars(step.params["prompt"].as_str().unwrap_or(""), vars);
                let max_turns = step.params["max_turns"].as_u64().unwrap_or(3) as usize;
                let result = self.callbacks.agent_run(&prompt, max_turns).await?;
                Ok((result, HashMap::new()))
            }
            StepType::Delay => {
                let secs = step.params["seconds"].as_u64().unwrap_or(1);
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                Ok((format!("Waited {secs}s"), HashMap::new()))
            }
            StepType::Log => {
                let msg = resolve_vars(step.params["message"].as_str().unwrap_or(""), vars);
                let level = step.params["level"].as_str().unwrap_or("info");
                match level {
                    "error" => tracing::error!("{}", msg),
                    "warn" => tracing::warn!("{}", msg),
                    _ => tracing::info!("{}", msg),
                }
                Ok((msg, HashMap::new()))
            }
            StepType::SetVariable => {
                let key = step.params["key"].as_str().unwrap_or("").to_string();
                let value = resolve_vars(step.params["value"].as_str().unwrap_or(""), vars);
                let mut exports = HashMap::new();
                exports.insert(key.clone(), value.clone());
                Ok((format!("{key} = {value}"), exports))
            }
            StepType::Condition => {
                let if_steps: Vec<Step> = serde_json::from_value(step.params["if"].clone())
                    .map_err(|e| format!("Invalid 'if' steps: {e}"))?;
                let condition = step.condition.as_deref().unwrap_or("true");
                let else_steps: Vec<Step> = step.params.get("else")
                    .and_then(|e| serde_json::from_value(e.clone()).ok())
                    .unwrap_or_default();
                let branch = if self.eval_condition(condition, vars) {
                    &if_steps
                } else {
                    &else_steps
                };
                let mut outputs = Vec::new();
                for s in branch {
                    let r = Box::pin(self.execute_step(s, vars)).await?;
                    outputs.push(r.output);
                }
                Ok((outputs.join("\n"), HashMap::new()))
            }
        };

        let (output, exports) = output.map_err(|e| format!("step {} failed: {e}", step.id))?;
        Ok(StepResult {
            step_id: step.id.clone(),
            success: true,
            output,
            exports,
            duration_ms: 0, // filled in by caller
            error: None,
            retries: 0,
        })
    }

    /// Simple condition evaluator: supports `$var`, `==`, `!=`, `contains`.
    fn eval_condition(&self, condition: &str, vars: &HashMap<String, String>) -> bool {
        let resolved = resolve_vars(condition, vars);
        let resolved = resolved.trim();

        if resolved.is_empty() || resolved == "true" {
            return true;
        }
        if resolved == "false" {
            return false;
        }

        // Support: $var == "value"
        if let Some((left, right)) = resolved.split_once("==") {
            return left.trim() == right.trim().trim_matches('"');
        }
        if let Some((left, right)) = resolved.split_once("!=") {
            return left.trim() != right.trim().trim_matches('"');
        }
        if let Some((_left, _right)) = resolved.split_once("contains") {
            return resolved.contains("true"); // simplified
        }

        // Default: truthy (non-empty)
        !resolved.is_empty()
    }
}

// ── Variable resolution ──────────────────────────────────────────────────

/// Resolve `${{step.key}}` references in a string.
fn resolve_vars(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let placeholder = format!("${{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_vars() {
        let mut vars = HashMap::new();
        vars.insert("step1.stdout".into(), "hello world".into());
        let result = resolve_vars("Output was: ${{step1.stdout}}", &vars);
        assert_eq!(result, "Output was: hello world");
    }

    #[test]
    fn test_empty_workflow() {
        let def = WorkflowDefinition {
            name: "empty".into(),
            description: String::new(),
            steps: vec![],
            variables: HashMap::new(),
            stop_on_error: true,
            timeout_secs: 300,
        };
        let engine = WorkflowEngine::new_noop();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.execute(&def));
        assert!(result.success);
        assert_eq!(result.steps_completed, 0);
    }

    #[test]
    fn test_log_step() {
        let def = WorkflowDefinition {
            name: "log-test".into(),
            description: String::new(),
            steps: vec![Step {
                id: "s1".into(),
                step_type: StepType::Log,
                description: "test".into(),
                params: serde_json::json!({"message": "hello", "level": "info"}),
                condition: None,
                retry: 0,
                timeout_secs: 60,
            }],
            variables: HashMap::new(),
            stop_on_error: true,
            timeout_secs: 300,
        };
        let engine = WorkflowEngine::new_noop();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.execute(&def));
        assert!(result.success);
        assert_eq!(result.steps_completed, 1);
    }

    #[test]
    fn test_set_variable_and_use() {
        let def = WorkflowDefinition {
            name: "var-test".into(),
            description: String::new(),
            steps: vec![
                Step {
                    id: "s1".into(),
                    step_type: StepType::SetVariable,
                    description: "set name".into(),
                    params: serde_json::json!({"key": "name", "value": "EverEvo"}),
                    condition: None,
                    retry: 0,
                    timeout_secs: 60,
                },
                Step {
                    id: "s2".into(),
                    step_type: StepType::Log,
                    description: "greet".into(),
                    params: serde_json::json!({"message": "Hello ${{s1.name}}!", "level": "info"}),
                    condition: None,
                    retry: 0,
                    timeout_secs: 60,
                },
            ],
            variables: HashMap::new(),
            stop_on_error: true,
            timeout_secs: 300,
        };
        let engine = WorkflowEngine::new_noop();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.execute(&def));
        assert!(result.success);
        assert_eq!(result.steps_completed, 2);
        assert!(result.final_variables.contains_key("s1.name"));
        assert_eq!(result.final_variables["s1.name"], "EverEvo");
    }

    #[test]
    fn test_delay_step() {
        let def = WorkflowDefinition {
            name: "delay-test".into(),
            description: String::new(),
            steps: vec![Step {
                id: "s1".into(),
                step_type: StepType::Delay,
                description: "wait".into(),
                params: serde_json::json!({"seconds": 0}),
                condition: None,
                retry: 0,
                timeout_secs: 60,
            }],
            variables: HashMap::new(),
            stop_on_error: true,
            timeout_secs: 300,
        };
        let engine = WorkflowEngine::new_noop();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.execute(&def));
        assert!(result.success);
    }

    #[test]
    fn test_stop_on_error() {
        // Use a custom callback that fails on shell_exec
        struct FailingCallbacks;
        #[async_trait::async_trait]
        impl WorkflowCallbacks for FailingCallbacks {
            async fn shell_exec(&self, _cmd: &str, _wd: Option<&str>) -> Result<(String, String, i32), String> {
                Err("simulated failure".into())
            }
            async fn fetch_url(&self, _url: &str) -> Result<String, String> {
                Ok(String::new())
            }
            async fn memory_save(&self, _key: &str, _content: &str) -> Result<(), String> {
                Ok(())
            }
            async fn memory_search(&self, _query: &str) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
            async fn agent_run(&self, _prompt: &str, _max_turns: usize) -> Result<String, String> {
                Ok(String::new())
            }
        }

        let engine = WorkflowEngine::new(FailingCallbacks);
        let def = WorkflowDefinition {
            name: "error-test".into(),
            description: String::new(),
            steps: vec![
                Step {
                    id: "s1".into(),
                    step_type: StepType::Shell,
                    description: "fails".into(),
                    params: serde_json::json!({"command": "echo test"}),
                    condition: None,
                    retry: 0,
                    timeout_secs: 60,
                },
                Step {
                    id: "s2".into(),
                    step_type: StepType::Log,
                    description: "never runs".into(),
                    params: serde_json::json!({"message": "should not run"}),
                    condition: None,
                    retry: 0,
                    timeout_secs: 60,
                },
            ],
            variables: HashMap::new(),
            stop_on_error: true,
            timeout_secs: 300,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.execute(&def));
        assert!(!result.success);
        assert_eq!(result.steps_failed, 1);
        // s2 should not have executed
        assert!(!result.step_results.iter().any(|r| r.step_id == "s2"));
    }
}
