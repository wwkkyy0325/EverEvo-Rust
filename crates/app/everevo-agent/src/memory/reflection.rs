//! Reflection agent — Reflexion-pattern self-critique after each turn.
//!
//! Reuses the extractor's `llm.chat → JSON` pattern, but the prompt asks for
//! *lessons* (what to do differently) rather than facts. Output lands as
//! `FactType::Feedback`, which `MemoryStage` surfaces automatically next time
//! (zero-wiring reuse — the recall-promotion ladder handles surfacing).
//!
//! Sibling of `extract_from_turn` — same fire-and-forget shape, same parser.

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::FactType;

use crate::llm::HttpClient;
use crate::memory::extractor::parse_extracted_facts;
use crate::memory::facts::FactManager;

/// Reflect on a completed turn and sediment lessons as `Feedback` facts.
///
/// Runs as a background tokio task (caller spawns it). Failures are logged,
/// never surfaced. Only runs when the turn has substance.
pub async fn reflect_on_turn(
    llm: &HttpClient,
    fact_manager: &FactManager,
    user_msg: &str,
    assistant_msg: &str,
) {
    // Reflexion (Shinn et al. 2023): reflect once per meaningful episode,
    // analyzing the full trajectory. We model each turn as a mini-episode
    // and trigger deterministically when the exchange has substance.
    // Skip trivial turns; the LLM produces `[NO_LESSONS]` for non-actionable
    // content, so overlarge triggers only waste tokens, not correctness.
    if user_msg.len() < 20 || assistant_msg.len() < 20 {
        return;
    }

    let prompt = build_reflection_prompt(user_msg, assistant_msg);
    let messages = vec![LlmMessage::user(&prompt)];

    match llm.chat(&messages, &[]).await {
        Ok(response) => {
            let text = response.content.unwrap_or_default();
            if text.is_empty() || text.contains("[NO_LESSONS]") {
                return;
            }
            let lessons = parse_extracted_facts(&text);
            if lessons.is_empty() {
                return;
            }
            let mut saved = 0u32;
            for (name, description, content) in &lessons {
                // `lesson-` prefix distinguishes reflection output from raw
                // extracted facts and avoids name collisions.
                let fact = everevo_core::memory::MemoryFact {
                    name: format!("lesson-{name}"),
                    description: description.clone(),
                    content: content.clone(),
                    fact_type: FactType::Feedback,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    projection: everevo_core::memory::ProjectionMetadata::new(
                        "reflection-agent",
                        "llm",
                        vec![],
                        0.8,
                    ),
                    links: vec![],
                    // Reflection lessons are cross-session reusable — global tier.
                    session: Some("global".into()),
                };
                match fact_manager.save_async(fact.clone()).await {
                    Ok(()) => saved += 1,
                    Err(e) => tracing::debug!(
                        name = %fact.name,
                        error = %e,
                        "Reflection lesson dedup'd or rejected"
                    ),
                }
            }
            if saved > 0 {
                tracing::info!(saved, "Reflection agent sedimented lessons");
            }
        }
        Err(e) => tracing::debug!(error = %e, "Reflection LLM call failed"),
    }
}

/// Summary agent: if the completed turn looks like a repeatable multi-step
/// procedure, ask the LLM to distill it into a `WorkflowDefinition` and save
/// it to the library — so future sessions skip discovery (`workflow_run name=`).
///
/// Fire-and-forget like `reflect_on_turn`. No-op on one-off/trivial turns
/// (LLM returns `[NOT_REUSABLE]`).
pub async fn compose_workflow_if_reusable(
    llm: &HttpClient,
    workflows_dir: &std::path::Path,
    user_msg: &str,
    assistant_msg: &str,
) {
    // Agent Workflow Memory (Wang et al. 2024, ICML 2025): extract workflows
    // from successfully completed tasks, not from every turn. We model each
    // substantial turn as a mini-task and apply:
    // 1. Success gate — skip if the response looks like an error/failure
    // 2. Turn counter — only attempt every 5th substantial turn (avoids
    //    spamming the LLM while still firing ~once per real session)
    if user_msg.len() < 30 || assistant_msg.len() < 200 {
        return;
    }
    // Success gate: skip error/failure turns (AWM extracts from successes)
    if is_error_response(assistant_msg) {
        tracing::debug!("Auto-compose: skipped — response looks like an error");
        return;
    }
    // Turn counter: fire every 5th substantial turn
    if !should_compose_now() {
        return;
    }
    let runner = crate::tools::builtins::WorkflowRunnerTool::new()
        .with_workflows_dir(workflows_dir.to_path_buf());
    // De-dup: skip if a workflow covering this task already exists.
    let msg_lower = user_msg.to_lowercase();
    let already_covered = runner
        .list_saved()
        .unwrap_or_default()
        .iter()
        .any(|(_, desc)| {
            desc.to_lowercase()
                .split_whitespace()
                .any(|kw| kw.len() > 3 && msg_lower.contains(kw))
        });
    if already_covered {
        tracing::debug!("Auto-compose: a similar workflow already exists — skipping");
        return;
    }
    let prompt = build_compose_prompt(user_msg, assistant_msg);
    let messages = vec![LlmMessage::user(&prompt)];
    let Ok(response) = llm.chat(&messages, &[]).await else {
        return;
    };
    let text = response.content.unwrap_or_default();
    if text.is_empty() || text.contains("[NOT_REUSABLE]") {
        return;
    }
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let def: everevo_workflow::WorkflowDefinition = match serde_json::from_str(cleaned) {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(error = %e, "Auto-compose: workflow JSON parse failed (skipped)");
            return;
        }
    };
    if def.steps.len() < 2 {
        return; // not enough steps to be worth saving
    }
    let slug = slugify(&def.name);
    if slug.is_empty() {
        return;
    }
    match runner.save_workflow(&slug, &def) {
        Ok(_) => tracing::info!(name = %slug, "Summary agent: auto-composed reusable workflow"),
        Err(e) => tracing::debug!(error = %e, "Auto-compose: save failed"),
    }
}

/// Lowercase + dash-join a free-form name into a workflow slug.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn build_compose_prompt(user_msg: &str, assistant_msg: &str) -> String {
    format!(
        "Analyze this completed task. Is it a REPEATABLE multi-step procedure worth saving as a \
         reusable workflow for future sessions?\n\n\
         If YES, return ONLY a JSON workflow definition:\n\
         {{\"name\": \"kebab-case-name\", \"description\": \"what it does\", \"steps\": [{{\"id\": \"s1\", \"type\": \"shell|fetch|memory_save|memory_search|agent|delay|log|set_variable\", \"description\": \"...\", \"params\": {{...}}}}]}}\n\n\
         If NO (one-off, trivial, conversational, or too ad-hoc), return exactly: [NOT_REUSABLE]\n\n\
         Save only when the task had 3+ steps another session would plausibly repeat.\n\n\
         User: {user_msg}\n\nAssistant: {assistant_msg}\n\nAnswer:"
    )
}

/// Turn-counter throttle: returns `true` every 5th call (roughly once per
/// real session). Uses a static atomic counter rather than probabilistic
/// sampling so the spacing is deterministic — AWM extracts workflows after
/// completed tasks, not randomly.
fn should_compose_now() -> bool {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed) % 5 == 0
}

/// Quick heuristic: does the assistant response look like an error/failure?
/// AWM extracts workflows from successful trajectories only.
fn is_error_response(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("error:")
        || lower.contains("failed:")
        || lower.contains("cannot")
        || lower.contains("unable to")
        || lower.contains("i couldn't")
        || lower.contains("blocked")
        || lower.contains("permission denied")
        || lower.contains("not available")
        || lower.len() < 100 // very short = likely truncation/error
}

fn build_reflection_prompt(user_msg: &str, assistant_msg: &str) -> String {
    format!(
        "Reflect on this completed task turn. Extract LESSONS that would help a future \
         session do this better or faster. Return ONLY a JSON array.\n\
         Each lesson: {{\"name\": \"kebab-case-slug\", \"description\": \"one sentence\", \
         \"content\": \"actionable guidance for next time\"}}\n\n\
         Focus on:\n\
         - Did the goal get achieved? If not, the root cause to avoid.\n\
         - Wasted steps / failed approaches to NOT repeat.\n\
         - Effective approaches WORTH repeating.\n\
         - Tool or command quirks, environment specifics discovered.\n\n\
         Skip trivial turns (greetings, simple lookups). Return [] or [NO_LESSONS] if \
         nothing reusable.\n\n\
         User: {user_msg}\n\nAssistant: {assistant_msg}\n\nJSON:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reflection_prompt_asks_for_lessons() {
        let p = build_reflection_prompt("do X", "did X via Y");
        assert!(p.contains("LESSONS"));
        assert!(p.contains("WORTH repeating"));
        assert!(p.contains("do X"));
    }

    #[test]
    fn test_reflection_reuses_extractors_parser() {
        // Same JSON shape as the extractor -> same parser works unchanged.
        let json = r#"[{"name":"avoid-ssh","description":"SSH blocked","content":"Use HTTPS not SSH for git"}]"#;
        let lessons = parse_extracted_facts(json);
        assert_eq!(lessons.len(), 1);
        assert_eq!(lessons[0].0, "avoid-ssh");
    }

    #[test]
    fn test_slugify_normalizes_name() {
        assert_eq!(slugify("Deploy the App!"), "deploy-the-app");
        assert_eq!(slugify("web_research v2"), "web-research-v2");
        assert!(slugify("!!!").is_empty());
    }

    #[test]
    fn test_compose_prompt_asks_for_reusability() {
        let p = build_compose_prompt("do multi-step X", "did step1, step2, step3");
        assert!(p.contains("REPEATABLE"));
        assert!(p.contains("[NOT_REUSABLE]"));
        assert!(p.contains("do multi-step X"));
    }
}
