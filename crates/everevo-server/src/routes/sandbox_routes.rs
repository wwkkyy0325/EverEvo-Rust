//! Sandbox API — status, shells, trust, audit, dreaming.

use axum::{
    extract::{Path, Query, State},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::app_state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/sandbox/status", get(status))
        .route("/api/sandbox/shells", get(available_shells))
        .route("/api/sandbox/sessions/{id}/trust", get(get_trusted).post(add_trusted))
        .route("/api/sandbox/sessions/{id}/audit", get(get_audit))
        .route("/api/sandbox/sessions/{id}/permission", put(set_permission))
        .route("/api/sandbox/sessions/{id}/confirm", post(confirm_command))
        .route("/api/sandbox/dreaming", get(dreaming_status))
        .route("/api/sandbox/dreaming/trigger", post(dreaming_trigger))
        .route("/api/memory/facts", get(list_facts))
        .route("/api/memory/facts/{name}", delete(delete_fact))
        .route("/api/agent/delegate", post(agent_delegate))
        .route("/api/agent/pool", get(agent_pool_status))
}

#[derive(Deserialize)]
struct TrustRequest { path: String }

#[derive(Deserialize)]
struct AuditQuery { limit: Option<usize> }

#[derive(Deserialize)]
struct PermissionRequest {
    level: String, // "read_only" | "fully_manual" | "semi_auto" | "fully_auto"
}

#[derive(Deserialize)]
struct ConfirmRequest {
    approved: bool,
}

async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let sandboxes = state.sandboxes.read().await;
    let active_count = sandboxes.len();
    let sample = sandboxes.values().next();
    let (shell_name, permission_key, permission_label) = match sample {
        Some(sb) => {
            let level = sb.permission_level();
            (sb.engine().shell_name().to_string(), level_key(level).to_string(), level.label().to_string())
        }
        None => ("none".into(), "semi_auto".into(), "—".into()),
    };
    let shells = everevo_sandbox::ShellResolver::detect_all();
    Json(serde_json::json!({
        "data": {
            "active_sessions": active_count,
            "shell": shell_name,
            "permission_level": permission_label,
            "permission_key": permission_key,
            "available_shells": shells.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "available_levels": [
                { "key": "read_only", "label": "只读" },
                { "key": "fully_manual", "label": "纯手动" },
                { "key": "semi_auto", "label": "半自动" },
                { "key": "fully_auto", "label": "全自动" },
            ]
        }
    }))
}

async fn available_shells(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let shells = everevo_sandbox::ShellResolver::detect_all();
    let list: Vec<_> = shells.iter().map(|s| serde_json::json!({
        "name": s.name, "executable": s.executable.display().to_string(),
    })).collect();
    Json(serde_json::json!({ "data": list }))
}

async fn add_trusted(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<TrustRequest>,
) -> Json<serde_json::Value> {
    let sandboxes = state.sandboxes.read().await;
    match sandboxes.get(&session_id) {
        Some(sb) => { sb.trust_path(&body.path); Json(serde_json::json!({ "data": { "trusted": true } })) }
        None => Json(serde_json::json!({ "error": "not found" })),
    }
}

async fn get_trusted(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let sandboxes = state.sandboxes.read().await;
    match sandboxes.get(&session_id) {
        Some(sb) => Json(serde_json::json!({ "data": { "trusted_paths": sb.trusted_paths() } })),
        None => Json(serde_json::json!({ "error": "not found" })),
    }
}

async fn get_audit(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> Json<serde_json::Value> {
    let sandboxes = state.sandboxes.read().await;
    match sandboxes.get(&session_id) {
        Some(sb) => {
            // Read audit.jsonl from the session sandbox dir
            let path = sb.sandbox_dir().join("audit.jsonl");
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let limit = q.limit.unwrap_or(100);
                    let lines: Vec<&str> = content.lines().rev().take(limit).collect();
                    let records: Vec<serde_json::Value> = lines.iter()
                        .filter_map(|l| serde_json::from_str(l).ok())
                        .collect();
                    Json(serde_json::json!({ "data": { "records": records, "total": content.lines().count() } }))
                }
                Err(_) => Json(serde_json::json!({ "data": { "records": [], "total": 0 } })),
            }
        }
        None => Json(serde_json::json!({ "data": { "records": [], "total": 0 } })),
    }
}

/// Change the permission level for a session's sandbox.
///
/// Accepts: `{ "level": "semi_auto" | "fully_manual" | "read_only" | "fully_auto" }`
async fn set_permission(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<PermissionRequest>,
) -> Json<serde_json::Value> {
    let level = match body.level.as_str() {
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "semi_auto" => everevo_sandbox::PermissionLevel::SemiAuto,
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        other => {
            return Json(serde_json::json!({
                "error": format!("Unknown permission level: {other}. Valid: read_only, fully_manual, semi_auto, fully_auto")
            }));
        }
    };

    let mut sandboxes = state.sandboxes.write().await;
    match sandboxes.get_mut(&session_id) {
        Some(sb) => {
            sb.set_permission_level(level);
            tracing::info!(%session_id, level = %level.label(), "Permission level changed");
            Json(serde_json::json!({
                "data": {
                    "session_id": session_id.to_string(),
                    "permission_level": level.label(),
                    "permission_key": body.level,
                }
            }))
        }
        None => Json(serde_json::json!({ "error": "Session not found" })),
    }
}

/// Map PermissionLevel to its API key string.
fn level_key(level: everevo_sandbox::PermissionLevel) -> &'static str {
    match level {
        everevo_sandbox::PermissionLevel::ReadOnly => "read_only",
        everevo_sandbox::PermissionLevel::FullyManual => "fully_manual",
        everevo_sandbox::PermissionLevel::SemiAuto => "semi_auto",
        everevo_sandbox::PermissionLevel::FullyAuto => "fully_auto",
    }
}

/// Resolve a pending confirmation — called when the user clicks Allow/Deny
/// in the frontend permission dialog.
///
/// This fires the oneshot sender that the blocked ShellTool is awaiting.
async fn confirm_command(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<ConfirmRequest>,
) -> Json<serde_json::Value> {
    let pending = state.confirmations.write().await.remove(&session_id);

    match pending {
        Some(p) => {
            let _ = p.response_tx.send(body.approved);
            tracing::info!(
                %session_id,
                approved = body.approved,
                command = %p.command,
                "User confirmed/denied command"
            );
            Json(serde_json::json!({
                "data": {
                    "session_id": session_id.to_string(),
                    "approved": body.approved,
                    "command": p.command,
                }
            }))
        }
        None => Json(serde_json::json!({
            "error": "No pending confirmation for this session"
        })),
    }
}

// ── Dreaming Scheduler API ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct DreamingTriggerRequest {
    phase: String, // "light" | "rem" | "deep" | "all"
}

/// GET /api/sandbox/dreaming — return the current scheduler status.
async fn dreaming_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let sched = &state.scheduler;
    Json(serde_json::json!({
        "data": {
            "running": sched.is_running(),
            "last_light_ts": sched.last_light_ts(),
            "last_rem_ts": sched.last_rem_ts(),
            "last_deep_ts": sched.last_deep_ts(),
            "turn_counter": sched.turn_count(),
        }
    }))
}

/// POST /api/sandbox/dreaming/trigger — manually trigger a dreaming phase.
async fn dreaming_trigger(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DreamingTriggerRequest>,
) -> Json<serde_json::Value> {
    use everevo_agent::memory::scheduler::ScheduledPhase;

    let phases: Vec<ScheduledPhase> = match body.phase.as_str() {
        "light" => vec![ScheduledPhase::Light { reason: "manual".into() }],
        "rem" => vec![ScheduledPhase::Rem],
        "deep" => vec![ScheduledPhase::Deep],
        "all" => vec![
            ScheduledPhase::Light { reason: "manual".into() },
            ScheduledPhase::RemAndDeep,
        ],
        other => {
            return Json(serde_json::json!({
                "error": format!("Unknown phase: {other}. Valid: light, rem, deep, all")
            }));
        }
    };

    let mut results = Vec::new();
    for phase in &phases {
        let phase_name = format!("{:?}", phase);
        match state.scheduler.trigger_phase(phase, &state.dreaming_engine).await {
            Ok(()) => {
                tracing::info!(%phase_name, "Manual dreaming phase completed");
                results.push(serde_json::json!({ "phase": phase_name, "status": "ok" }));
            }
            Err(e) => {
                tracing::warn!(%phase_name, error = %e, "Manual dreaming phase failed");
                results.push(serde_json::json!({ "phase": phase_name, "status": "error", "error": e.to_string() }));
            }
        }
    }

    Json(serde_json::json!({
        "data": {
            "triggered": body.phase,
            "results": results,
        }
    }))
}

// ── Memory Facts API ────────────────────────────────────────────

async fn list_facts(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match state.fact_manager.load_all() {
        Ok(facts) => {
            let items: Vec<_> = facts.iter().map(|f| serde_json::json!({
                "name": f.name, "description": f.description,
                "fact_type": f.fact_type.as_str(),
                "created_at": f.created_at.to_rfc3339(),
                "updated_at": f.updated_at.to_rfc3339(),
            })).collect();
            Json(serde_json::json!({ "data": { "facts": items, "total": items.len() } }))
        }
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn delete_fact(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Json<serde_json::Value> {
    match state.fact_manager.delete(&name) {
        Ok(()) => Json(serde_json::json!({ "data": { "deleted": true, "name": name } })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

// ── Agent Orchestration API ──────────────────────────────────────

#[derive(Deserialize)]
struct AgentDelegateRequest { task: String, persona: Option<String> }

async fn agent_delegate(State(state): State<Arc<AppState>>, Json(body): Json<AgentDelegateRequest>) -> Json<serde_json::Value> {
    let guard = state.llm.read().await;
    let client = guard.values().find_map(|c| c.as_ref()).cloned();
    drop(guard);
    let Some(client) = client else {
        return Json(serde_json::json!({ "error": "No LLM configured" }));
    };
    let tools = Arc::new(everevo_core::tool::ToolRegistry::new());
    let mut supervisor = everevo_agent::orchestration::SupervisorAgent::new(
        tools, state.config.data_dir.join("sandbox"),
    );
    let result = supervisor.orchestrate(&body.task, client, supervisor.tool_registry.clone(), body.persona, vec![]).await;
    Json(serde_json::json!({ "data": { "result": result.content, "subtasks": result.subtask_results.len(), "re_plans": result.re_plans, "duration_ms": result.duration_ms }}))
}

async fn agent_pool_status(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "data": { "max_concurrent": 3, "status": "operational" }}))
}