//! Shared init logic used by both the CLI binary (`main.rs`) and the
//! Tauri desktop binary (`src-tauri/src/main.rs`).

use crate::app_state::{AppState, InitPhase};
use std::sync::Arc;

/// Phase 2–4 of init: check LLM availability, wait for config if missing,
/// then run startup self-checks.
pub async fn run_init_llm_phase(state: &Arc<AppState>, data_dir: &std::path::Path) {
    let llm_count = state
        .llm
        .read()
        .await
        .values()
        .filter(|c| c.is_some())
        .count();
    println!("[init] LLM check: {} configured provider(s)", llm_count);

    if llm_count == 0 {
        println!("[init] No LLM configured — will prompt in-app after startup");
        tracing::info!("No LLM provider configured — user will be prompted in-app");
    }

    // ── Run startup self-checks ────────────────────────────────────
    println!("[init] Running startup checks (phase → Checking)");
    *state.init_phase.write().await = InitPhase::Checking;
    let report = crate::startup_check::run_startup_check(data_dir).await;
    if report.fail > 0 {
        tracing::error!(fail = report.fail, "Startup check found critical issues");
    }
    tracing::info!("Init complete — all systems ready");

    println!("[init] Init complete (phase → Ready)");
    *state.init_phase.write().await = InitPhase::Ready;
}
