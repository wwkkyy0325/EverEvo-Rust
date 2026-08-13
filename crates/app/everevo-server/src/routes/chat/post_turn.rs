//! Post-turn background tasks (fire-and-forget with panic guards).
//!
//! Each task is spawned independently. Panics are caught and logged — a single
//! failing task does not affect the others or the main conversation.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;

use crate::app_state::AppState;

/// Spawn a background task that catches panics and logs them instead of
/// silently killing the task.
fn spawn_guarded(
    label: &'static str,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::spawn(async move {
        if let Err(panic) = AssertUnwindSafe(future).catch_unwind().await {
            let msg = panic_message(&panic);
            tracing::error!(%label, %msg, "Post-turn background task panicked");
        }
    });
}

/// Extract a human-readable message from a panic payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Spawn all post-turn background tasks:
/// memory extraction, reflection, workflow auto-compose, persona update.
pub(super) async fn spawn_post_turn_tasks(
    state: &Arc<AppState>,
    session_id: uuid::Uuid,
    user_msg: &str,
    assistant_msg: &str,
) {
    let llm = state.llm.read().await;
    let Some(primary) = llm.values().find_map(|v| v.clone()) else {
        return;
    };
    let fm = state.fact_manager.clone();
    let um = user_msg.to_string();
    let am = assistant_msg.to_string();
    // Benchmark mode (EVEREVO_BENCHMARK=1): skip the GLOBAL-tier writers
    // (reflection lessons, workflow recipes, persona profile) so one GAIA
    // question's answer never leaks into later questions' context. Memory
    // extraction stays — it is session-scoped and strictly isolated.
    let benchmark = std::env::var("EVEREVO_BENCHMARK").is_ok();

    // Memory extraction (Mem0 pattern: durable facts) — tagged with the
    // originating session so cross-session recall stays strictly isolated.
    spawn_guarded("memory_extraction", {
        let p = Arc::clone(&primary);
        let fm = Arc::clone(&fm);
        let um = um.clone();
        let am = am.clone();
        async move {
            everevo_agent::memory::extractor::extract_from_turn(
                &p,
                &fm,
                Some(session_id),
                &um,
                &am,
            )
            .await;
        }
    });

    // Reflection (Reflexion pattern: lessons → feedback facts)
    if !benchmark {
        spawn_guarded("reflection", {
            let p = Arc::clone(&primary);
            let fm = Arc::clone(&fm);
            let um = um.clone();
            let am = am.clone();
            async move {
                everevo_agent::memory::reflection::reflect_on_turn(&p, &fm, &um, &am).await;
            }
        });
    }

    // Workflow auto-compose (repeatable task → reusable workflow)
    // Clone primary BEFORE moving into the closure chain
    if !benchmark {
        let p_wf = Arc::clone(&primary);
        spawn_guarded("workflow_compose", {
            let dir = state.config.data_dir.join("workflows");
            let um = um.clone();
            let am = am.clone();
            async move {
                everevo_agent::memory::reflection::compose_workflow_if_reusable(
                    &p_wf, &dir, &um, &am,
                )
                .await;
            }
        });
    }

    // Problem-model distillation (方案沉淀): if the session ended with a
    // FINALIZED structural problem model, distill the GENERIC causal-draft
    // process (the node/edge structure, NOT the specific question content —
    // anti-contamination) into a reusable workflow. Lightweight: a fixed
    // template of Agent steps; the existing `workflow_compose` above handles
    // content-specific workflow extraction.
    if !benchmark {
        let pm_read = state.problem_models.read().await;
        let finalized = pm_read
            .get(&session_id)
            .is_some_and(|m| m.finalized && !m.is_empty());
        drop(pm_read);
        if finalized {
            spawn_guarded("problem_model_distill", {
                let dir = state.config.data_dir.join("workflows");
                async move {
                    let runner = everevo_agent::tools::builtins::WorkflowRunnerTool::new()
                        .with_workflows_dir(dir);
                    let def_json = serde_json::json!({
                        "name": "causal-draft-problem-modeling",
                        "description": "Generic causal-draft problem modeling for complex questions: decompose into sub-questions, tag epistemic status, research + verify each, synthesize.",
                        "steps": [
                            {"id": "decompose", "type": "agent", "description": "Decompose the question into sub-questions / facts / claims; tag each VERIFIED/UNVERIFIED/UNKNOWN.", "params": {"prompt": "Decompose the task into sub-questions and key claims. For each, note whether a retrieved source states it."}},
                            {"id": "research", "type": "agent", "description": "Search authoritative sources for each sub-question; record the source for every value.", "params": {"prompt": "Research each sub-question via authoritative sources and record the exact source for every value."}},
                            {"id": "verify", "type": "agent", "description": "Run the deterministic verifier, then adversarial review on disagreement.", "params": {"prompt": "Verify each candidate value: recompute numerics, check verbatim fidelity against the source, and escalate to adversarial review on disagreement."}},
                            {"id": "answer", "type": "agent", "description": "Finalize and answer each sub-question with its [VERIFIED] source.", "params": {"prompt": "Produce the final answer: each sub-question gets its value with its verified source."}}
                        ]
                    });
                    match serde_json::from_value(def_json) {
                        Ok(def) => {
                            if let Err(e) =
                                runner.save_workflow("causal-draft-problem-modeling", &def)
                            {
                                tracing::warn!(error = %e, "Failed to save distilled problem-modeling workflow");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to parse distilled workflow def")
                        }
                    }
                }
            });
        }
    }

    // Persona auto-update (evolves communication style from accumulated facts)
    if !benchmark {
        spawn_guarded("persona_update", {
            let profile_path = state
                .config
                .data_dir
                .join("memory")
                .join("persona")
                .join("profile.json");
            let fm = Arc::clone(&fm);
            async move {
                let facts = fm.load_all().unwrap_or_default();
                everevo_agent::stages::persona::update_persona_from_facts(&profile_path, &facts);
            }
        });
    }

    // Paradigm extraction (SAMULE pattern: extract reusable action patterns)
    spawn_guarded("paradigm_extraction", {
        let p = Arc::clone(&primary);
        let fm = Arc::clone(&fm);
        async move {
            let buffer = everevo_agent::memory::TrajectoryBuffer::default();
            everevo_agent::memory::extract_paradigm_from_trajectory(&p, &fm, &buffer).await;
        }
    });

    // Meta-Agent post-hoc paradigm consolidation check
    spawn_guarded("meta_agent_post", {
        let fm = Arc::clone(&fm);
        async move {
            let paradigms = everevo_agent::memory::load_paradigms(&fm);
            if paradigms.len() > 10 {
                tracing::info!(
                    count = paradigms.len(),
                    "Meta-agent post: {} paradigms accumulated",
                    paradigms.len()
                );
            }
        }
    });
}
