//! Memory control API — dreaming pipeline triggers and status.
//!
//! ## API Surface (OpenClaw-aligned)
//!
//! | Method | Path | Purpose |
//! |--------|------|---------|
//! | GET | `/api/memory/status` | Dreaming pipeline status + counts |
//! | POST | `/api/memory/dream` | Trigger a dreaming phase (light/rem/deep/all) |
//! | POST | `/api/memory/consolidate` | Run consolidation pass on existing facts |
//!
//! LLM can call these via the `memory` tool or the shell tool with curl.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::app_state::AppState;
use everevo_agent::memory::scheduler::ScheduledPhase;

#[derive(Deserialize)]
struct DreamTrigger {
    /// Phase to run: "light", "rem", "deep", "all" (LIGHT→REM→DEEP in sequence).
    phase: Option<String>,
}

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/memory/status", axum::routing::get(status))
        .route("/api/memory/dream", axum::routing::post(trigger_dream))
        .route("/api/memory/consolidate", axum::routing::post(trigger_consolidate))
}

async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let fact_count = state.fact_manager.count().unwrap_or(0);
    let diary_files = state.diary_manager.list_files().unwrap_or_default();
    let dreams_exists = state.config.data_dir.join("memory").join(".dreams").join("themes.jsonl").exists();
    let graph_exists = state.config.data_dir.join("memory").join("graph").exists();
    let wiki_count = std::fs::read_dir(state.config.data_dir.join("memory").join("wiki"))
        .map(|iter| iter.count())
        .unwrap_or(0);
    let has_llm = state.dreaming_engine.has_llm();
    let buffered = state.dreaming_engine.has_buffered_messages();

    Json(serde_json::json!({
        "pipeline": {
            "has_llm": has_llm,
            "buffered_messages": buffered,
            "scheduler_running": state.scheduler.is_running(),
        },
        "storage": {
            "facts": fact_count,
            "diary_files": diary_files.len(),
            "dreams_exists": dreams_exists,
            "graph_exists": graph_exists,
            "wiki_pages": wiki_count,
        },
        "timers": {
            "last_light_ts": state.scheduler.last_light_ts(),
            "last_rem_ts": state.scheduler.last_rem_ts(),
            "last_deep_ts": state.scheduler.last_deep_ts(),
            "turn_count": state.scheduler.turn_count(),
        },
    }))
}

async fn trigger_dream(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DreamTrigger>,
) -> Json<serde_json::Value> {
    let phase = body.phase.as_deref().unwrap_or("all");

    let result = match phase {
        "light" => {
            let messages = state.dreaming_engine.drain_messages();
            if messages.is_empty() {
                "No buffered messages to process. LIGHT phase skipped.".to_string()
            } else {
                let count = messages.len();
                match state.dreaming_engine.execute_light_with_messages("manual", &messages).await {
                    Ok(()) => format!("LIGHT phase completed — processed {count} messages"),
                    Err(e) => format!("LIGHT phase failed: {e}"),
                }
            }
        }
        "rem" => {
            match state.scheduler.trigger_phase(&ScheduledPhase::Rem, &state.dreaming_engine).await {
                Ok(()) => "REM phase completed — themes extracted from recent diary".into(),
                Err(e) => format!("REM phase failed: {e}"),
            }
        }
        "deep" => {
            match state.scheduler.trigger_phase(&ScheduledPhase::Deep, &state.dreaming_engine).await {
                Ok(()) => "DEEP phase completed — facts scored, consolidated, promoted".into(),
                Err(e) => format!("DEEP phase failed: {e}"),
            }
        }
        _ => { // "all" or any other value — run full pipeline
            let mut results = Vec::new();
            // LIGHT
            let messages = state.dreaming_engine.drain_messages();
            if messages.is_empty() {
                results.push("LIGHT: no buffered messages".to_string());
            } else {
                let count = messages.len();
                match state.dreaming_engine.execute_light_with_messages("manual", &messages).await {
                    Ok(()) => results.push(format!("LIGHT ✅ — {count} messages")),
                    Err(e) => results.push(format!("LIGHT ❌ — {e}")),
                }
            }
            // REM
            match state.scheduler.trigger_phase(&ScheduledPhase::Rem, &state.dreaming_engine).await {
                Ok(()) => results.push("REM ✅".into()),
                Err(e) => results.push(format!("REM ❌ — {e}")),
            }
            // DEEP
            match state.scheduler.trigger_phase(&ScheduledPhase::Deep, &state.dreaming_engine).await {
                Ok(()) => results.push("DEEP ✅".into()),
                Err(e) => results.push(format!("DEEP ❌ — {e}")),
            }
            results.join("\n")
        }
    };

    tracing::info!(%phase, "Dreaming phase triggered via API");
    Json(serde_json::json!({
        "phase": phase,
        "result": result,
    }))
}

async fn trigger_consolidate(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    // Run DEEP consolidator pass on existing facts without REM themes
    match state.scheduler.trigger_phase(&ScheduledPhase::Deep, &state.dreaming_engine).await {
        Ok(()) => Json(serde_json::json!({
            "status": "ok",
            "message": "Consolidation pass completed",
            "fact_count": state.fact_manager.count().unwrap_or(0),
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": format!("Consolidation failed: {e}"),
        })),
    }
}
