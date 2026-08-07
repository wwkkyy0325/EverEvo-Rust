//! ClusterTool — parallel agent orchestration for fan-out, map-reduce, and
//! adversarial verification. Uses SubAgentPool for bounded concurrency.
//!
//! Claude Code alignment: mirrors `parallel()`, `pipeline()`, and the
//! adversarial-verify quality pattern.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use crate::subagent_pool::{SubAgentPool, SubAgentTask};

/// Cluster-based parallel agent orchestration.
/// Supports fan_out, map_reduce, and verify patterns.
pub struct ClusterTool {
    pool: Option<Arc<SubAgentPool>>,
}

impl ClusterTool {
    pub fn new() -> Self {
        Self { pool: None }
    }

    /// Wire the sub-agent pool for actual execution.
    pub fn with_pool(mut self, pool: Arc<SubAgentPool>) -> Self {
        self.pool = Some(pool);
        self
    }
}

impl Default for ClusterTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ClusterTool {
    fn name(&self) -> &str {
        "cluster"
    }

    fn description(&self) -> &str {
        "Orchestrate multiple sub-agents in parallel using cluster patterns. \
         Actions: fan_out (N workers on the same task), \
         map_reduce (N workers → synthesize), \
         verify (adversarial verification with majority vote). \
         Parameters: action (required: fan_out/map_reduce/verify), \
         prompt (required), workers (default 3), \
         perspectives or claims (for verify), items (for map_reduce)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["fan_out", "map_reduce", "verify"],
                    "description": "Cluster pattern: fan_out (parallel workers), map_reduce (map+reduce phases), verify (adversarial check)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Task prompt for workers"
                },
                "workers": {
                    "type": "integer",
                    "description": "Number of parallel workers (default: 3, max: 10)"
                },
                "items": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Items for map phase (map_reduce only)"
                },
                "claims": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Claims to verify (verify action only)"
                },
                "perspectives": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Review perspectives, e.g. ['correctness', 'security', 'performance'] (verify and fan_out)"
                }
            },
            "required": ["action", "prompt"]
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
        let pool = self
            .pool
            .as_ref()
            .ok_or_else(|| EverEvoError::Internal("ClusterTool: no pool configured".into()))?;

        let action = params["action"].as_str().unwrap_or("fan_out");
        let prompt = params["prompt"].as_str().unwrap_or("");
        if prompt.is_empty() {
            return Ok(ToolOutput {
                content: "prompt is required".into(),
                is_error: true,
                ..Default::default()
            });
        }

        let workers = params["workers"].as_u64().unwrap_or(3).min(10) as usize;

        match action {
            "fan_out" => execute_fan_out(pool, prompt, workers).await,
            "map_reduce" => {
                let items: Vec<String> = params["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if items.is_empty() {
                    return Ok(ToolOutput {
                        content: "items array is required for map_reduce".into(),
                        is_error: true,
                        ..Default::default()
                    });
                }
                execute_map_reduce(pool, prompt, &items).await
            }
            "verify" => {
                let claims: Vec<String> = params["claims"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_else(|| vec![prompt.to_string()]);
                let perspectives: Vec<String> = params["perspectives"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        vec!["correctness".into(), "security".into(), "edge_cases".into()]
                    });
                execute_verify(pool, &claims, &perspectives).await
            }
            _ => Ok(ToolOutput {
                content: format!("Unknown action: {action}. Use fan_out, map_reduce, or verify."),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

// ── Cluster Patterns ───────────────────────────────────────────────────

/// Fan-out: N workers run the same prompt in parallel. Results are concatenated.
async fn execute_fan_out(
    pool: &SubAgentPool,
    prompt: &str,
    workers: usize,
) -> Result<ToolOutput, EverEvoError> {
    let tasks: Vec<SubAgentTask> = (0..workers)
        .map(|i| SubAgentTask {
            description: format!("worker-{i}"),
            prompt: format!(
                "{prompt}\n\n---\nYou are worker {i} of {workers}. \
                 Provide your independent analysis."
            ),
            max_turns: 5,
            system_prompt_override: None,
            cancel_token: None,
        })
        .collect();

    let results = pool.execute_all(tasks).await;

    let mut output = format!("## Fan-out: {workers} workers\n\n**Prompt:** {prompt}\n\n---\n\n");
    for r in &results {
        output.push_str(&format!(
            "### {} ({})\n\n{}\n\n---\n\n",
            r.description, r.status, r.content,
        ));
    }
    Ok(ToolOutput {
        content: output,
        is_error: false,
        ..Default::default()
    })
}

/// Map-reduce: each item goes to a worker, then a reducer synthesizes.
async fn execute_map_reduce(
    pool: &SubAgentPool,
    prompt: &str,
    items: &[String],
) -> Result<ToolOutput, EverEvoError> {
    // ── Map phase ──
    let map_tasks: Vec<SubAgentTask> = items
        .iter()
        .enumerate()
        .map(|(i, item)| SubAgentTask {
            description: format!("map-{i}"),
            prompt: format!("{prompt}\n\n---\nItem: {item}"),
            max_turns: 5,
            system_prompt_override: None,
            cancel_token: None,
        })
        .collect();

    let map_results = pool.execute_all(map_tasks).await;

    // ── Reduce phase ──
    let map_text: String = map_results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("### Result {i}\n{}\n", r.content))
        .collect::<Vec<_>>()
        .join("\n---\n\n");

    let reduce_tasks = vec![SubAgentTask {
        description: "reducer".into(),
        prompt: format!(
            "Synthesize the following parallel analysis results into a single, \
             coherent response. Identify agreements, resolve contradictions, \
             and highlight the most important findings.\n\n\
             Original prompt: {prompt}\n\n\
             ## Results to synthesize:\n\n{map_text}"
        ),
        max_turns: 8,
        system_prompt_override: Some(
            "You are a senior architect synthesizing results from multiple analysts. \
             Be concise but thorough. Resolve contradictions explicitly. \
             Flag any findings that appear unreliable."
                .into(),
        ),
        cancel_token: None,
    }];

    let reduce_results = pool.execute_all(reduce_tasks).await;
    let synthesis = reduce_results
        .first()
        .map(|r| r.content.clone())
        .unwrap_or_else(|| "Reduce phase failed.".into());

    Ok(ToolOutput {
        content: format!(
            "## Map-Reduce: {} items\n\n**Prompt:** {prompt}\n\n---\n\n## Synthesis\n\n{synthesis}",
            items.len(),
        ),
        is_error: false,
        ..Default::default()
    })
}

/// Adversarial verification: each claim is checked by multiple skeptics.
/// Survives if ≥ majority confirm it.
async fn execute_verify(
    pool: &SubAgentPool,
    claims: &[String],
    perspectives: &[String],
) -> Result<ToolOutput, EverEvoError> {
    let mut output = format!(
        "## Adversarial Verification: {} claims × {} perspectives\n\n",
        claims.len(),
        perspectives.len(),
    );

    for (i, claim) in claims.iter().enumerate() {
        let tasks: Vec<SubAgentTask> = perspectives
            .iter()
            .map(|persp| SubAgentTask {
                description: format!("{persp}-reviewer"),
                prompt: format!(
                    "Verify the following claim through the lens of **{persp}**:\n\n\
                     > {claim}\n\n\
                     Report: [CONFIRMED] if the claim holds, [REFUTED] if you find \
                     flaws, or [UNCERTAIN] if more information is needed. \
                     Provide specific evidence for your verdict."
                ),
                max_turns: 5,
                system_prompt_override: Some(format!(
                    "You are a {persp} reviewer. Be skeptical. Default to \
                     REFUTED if uncertain. Provide concrete evidence."
                )),
                cancel_token: None,
            })
            .collect();

        let results = pool.execute_all(tasks).await;
        let confirmed = results
            .iter()
            .filter(|r| r.content.to_uppercase().contains("CONFIRMED"))
            .count();
        let refuted = results
            .iter()
            .filter(|r| r.content.to_uppercase().contains("REFUTED"))
            .count();
        let majority = perspectives.len() / 2 + 1;
        let verdict = if confirmed >= majority {
            "✅ SURVIVES"
        } else if refuted >= majority {
            "❌ REFUTED"
        } else {
            "⚠️ UNCERTAIN"
        };

        output.push_str(&format!(
            "### Claim {i}: {verdict}\n> {claim}\n\n\
             Confirmed: {confirmed}/{}, Refuted: {refuted}/{}, Threshold: {majority}\n\n",
            perspectives.len(),
            perspectives.len(),
        ));
        for r in &results {
            let short: String = r.content.chars().take(300).collect();
            let is_truncated = r.content.chars().count() > 300;
            output.push_str(&format!(
                "- **{}**: {short}{}\n",
                r.description,
                if is_truncated { "..." } else { "" },
            ));
        }
        output.push_str("\n---\n\n");
    }

    Ok(ToolOutput {
        content: output,
        is_error: false,
        ..Default::default()
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_and_schema() {
        let tool = ClusterTool::new();
        assert_eq!(tool.name(), "cluster");
        assert_eq!(tool.risk_level(), RiskLevel::Medium);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "action");
        assert!(schema["properties"]["action"]["enum"].is_array());
    }
}
