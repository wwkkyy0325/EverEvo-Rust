//! Sandbox API — status, shells, trust, audit, dreaming.

use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::app_state::AppState;
use everevo_core::ApiError;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/sandbox/status", get(status))
        .route("/api/sandbox/shells", get(available_shells))
        .route(
            "/api/sandbox/sessions/{id}/trust",
            get(get_trusted).post(add_trusted),
        )
        .route("/api/sandbox/sessions/{id}/audit", get(get_audit))
        .route("/api/sandbox/sessions/{id}/permission", put(set_permission))
        .route("/api/sandbox/sessions/{id}/confirm", post(confirm_command))
        .route("/api/sandbox/dreaming", get(dreaming_status))
        .route("/api/sandbox/dreaming/trigger", post(dreaming_trigger))
        .route("/api/agent/tasks", get(list_subagent_tasks))
        .route("/api/agent/tasks/{id}/cancel", post(cancel_subagent))
        .route("/api/chat/{id}/interrupt", post(interrupt_chat))
        .route("/api/session/{id}/todos", get(get_session_todos))
}

#[derive(Deserialize)]
struct TrustRequest {
    path: String,
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

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
            (
                sb.engine().shell_name().to_string(),
                level_key(level).to_string(),
                level.label().to_string(),
            )
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
    let list: Vec<_> = shells
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name, "executable": s.executable.display().to_string(),
            })
        })
        .collect();
    Json(serde_json::json!({ "data": list }))
}

async fn add_trusted(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<TrustRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sandboxes = state.sandboxes.read().await;
    match sandboxes.get(&session_id) {
        Some(sb) => {
            sb.trust_path(&body.path);
            Ok(Json(serde_json::json!({ "data": { "trusted": true } })))
        }
        None => Err(ApiError::not_found("Sandbox session not found")),
    }
}

async fn get_trusted(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sandboxes = state.sandboxes.read().await;
    match sandboxes.get(&session_id) {
        Some(sb) => Ok(Json(
            serde_json::json!({ "data": { "trusted_paths": sb.trusted_paths() } }),
        )),
        None => Err(ApiError::not_found("Sandbox session not found")),
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
                    let records: Vec<serde_json::Value> = lines
                        .iter()
                        .filter_map(|l| serde_json::from_str(l).ok())
                        .collect();
                    Json(
                        serde_json::json!({ "data": { "records": records, "total": content.lines().count() } }),
                    )
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
) -> Result<Json<serde_json::Value>, ApiError> {
    let level = match body.level.as_str() {
        "read_only" => everevo_sandbox::PermissionLevel::ReadOnly,
        "fully_manual" => everevo_sandbox::PermissionLevel::FullyManual,
        "semi_auto" => everevo_sandbox::PermissionLevel::SemiAuto,
        "fully_auto" => everevo_sandbox::PermissionLevel::FullyAuto,
        other => {
            return Err(ApiError::bad_request(format!(
                "Unknown permission level: {other}. Valid: read_only, fully_manual, semi_auto, fully_auto"
            )));
        }
    };

    let mut sandboxes = state.sandboxes.write().await;
    match sandboxes.get_mut(&session_id) {
        Some(sb) => {
            sb.set_permission_level(level);
            tracing::info!(%session_id, level = %level.label(), "Permission level changed");

            // ── Auto-resolve pending confirmations when switching to FullyAuto ──
            // If a sub-agent (or the main agent) is blocked on a confirmation
            // oneshot, switching to FullyAuto should unblock it immediately.
            // Without this, mid-conversation permission changes can deadlock
            // already-spawned sub-agents that were waiting for user approval.
            if level == everevo_sandbox::PermissionLevel::FullyAuto {
                let pending = state.confirmations.write().await.remove(&session_id);
                if let Some(p) = pending {
                    let _ = p.response_tx.send(true); // auto-approve
                    tracing::info!(
                        %session_id,
                        command = %p.command,
                        "Auto-approved pending confirmation on FullyAuto switch"
                    );
                }
            }

            Ok(Json(serde_json::json!({
                "data": {
                    "session_id": session_id.to_string(),
                    "permission_level": level.label(),
                    "permission_key": body.level,
                }
            })))
        }
        None => Err(ApiError::not_found("Session not found")),
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
) -> Result<Json<serde_json::Value>, ApiError> {
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
            Ok(Json(serde_json::json!({
                "data": {
                    "session_id": session_id.to_string(),
                    "approved": body.approved,
                    "command": p.command,
                }
            })))
        }
        None => Err(ApiError::not_found(
            "No pending confirmation for this session",
        )),
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
) -> Result<Json<serde_json::Value>, ApiError> {
    use everevo_agent::memory::scheduler::ScheduledPhase;

    let phases: Vec<ScheduledPhase> = match body.phase.as_str() {
        "light" => vec![ScheduledPhase::Light {
            reason: "manual".into(),
        }],
        "rem" => vec![ScheduledPhase::Rem],
        "deep" => vec![ScheduledPhase::Deep],
        "all" => vec![
            ScheduledPhase::Light {
                reason: "manual".into(),
            },
            ScheduledPhase::RemAndDeep,
        ],
        other => {
            return Err(ApiError::bad_request(format!(
                "Unknown phase: {other}. Valid: light, rem, deep, all"
            )));
        }
    };

    let mut results = Vec::new();
    for phase in &phases {
        let phase_name = format!("{:?}", phase);
        match state
            .scheduler
            .trigger_phase(phase, &state.dreaming_engine)
            .await
        {
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

    Ok(Json(serde_json::json!({
        "data": {
            "triggered": body.phase,
            "results": results,
        }
    })))
}

// ── Memory Facts API ────────────────────────────────────────────

// ── Agent Orchestration API (deprecated — use /api/agent/tasks + TaskTool) ──

#[derive(Deserialize)]
#[allow(dead_code)]
struct AgentDelegateRequest {
    task: String,
    persona: Option<String>,
}

#[allow(dead_code)]
async fn agent_delegate(
    State(_state): State<Arc<AppState>>,
    Json(_body): Json<AgentDelegateRequest>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": { "result": "The /api/agent/delegate endpoint is deprecated. Use the Task tool in chat to spawn sub-agents.", "subtasks": 0 }
    }))
}

#[allow(dead_code)]
async fn agent_pool_status(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "data": { "max_concurrent": 3, "status": "operational" }}))
}

// ── Sub-agent Task API ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct SubagentQuery {
    #[serde(default)]
    session_id: Option<Uuid>,
}

/// GET /api/agent/tasks — list running and recently completed sub-agents.
async fn list_subagent_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SubagentQuery>,
) -> Json<serde_json::Value> {
    let statuses = state.subagent_statuses.read().await;

    // If session_id specified, return only that session's entries
    let items: Vec<serde_json::Value> = if let Some(sid) = q.session_id {
        statuses
            .get(&sid)
            .and_then(|arc| {
                let list = arc.lock().ok()?;
                Some(
                    list.iter()
                        .map(|s| serde_json::to_value(s).unwrap_or_default())
                        .collect(),
                )
            })
            .unwrap_or_default()
    } else {
        statuses
            .values()
            .flat_map(|arc| {
                let list = arc.lock().ok();
                list.map(|l| {
                    l.iter()
                        .map(|s| serde_json::to_value(s).unwrap_or_default())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            })
            .collect()
    };

    Json(serde_json::json!({
        "data": { "tasks": items, "total": items.len() }
    }))
}

/// POST /api/agent/tasks/{id}/cancel — cancel a running sub-agent.
async fn cancel_subagent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let handles = state.subagent_handles.read().await;
    for arc in handles.values() {
        if let Ok(list) = arc.lock() {
            if let Some(entry) = list.iter().find(|e| e.id == id) {
                entry.cancel.cancel();
                tracing::info!(%id, "Sub-agent cancelled via API");
                return Ok(Json(serde_json::json!({
                    "data": { "cancelled": true, "id": id.to_string() }
                })));
            }
        }
    }
    Err(ApiError::not_found(format!("Sub-agent not found: {id}")))
}

/// GET /api/session/{id}/todos — return current task list for a session.
async fn get_session_todos(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let store = state.todo_store.read().await;
    let todos = store.get(&session_id).cloned().unwrap_or_default();
    Json(serde_json::json!({
        "data": { "todos": todos }
    }))
}

/// POST /api/chat/{id}/interrupt — cancel the current agent turn.
/// Background sub-agents continue running; their results will appear
/// in the next conversation turn.
async fn interrupt_chat(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actors = state.session_actors.read().await;
    if let Some(token) = actors.get(&session_id) {
        token.cancel();
        tracing::info!(%session_id, "Agent run interrupted by user");
        Ok(Json(serde_json::json!({
            "data": { "interrupted": true, "session_id": session_id.to_string() }
        })))
    } else {
        Err(ApiError::not_found("No active agent run for this session"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_key_mapping() {
        assert_eq!(
            level_key(everevo_sandbox::PermissionLevel::ReadOnly),
            "read_only"
        );
        assert_eq!(
            level_key(everevo_sandbox::PermissionLevel::FullyManual),
            "fully_manual"
        );
        assert_eq!(
            level_key(everevo_sandbox::PermissionLevel::SemiAuto),
            "semi_auto"
        );
        assert_eq!(
            level_key(everevo_sandbox::PermissionLevel::FullyAuto),
            "fully_auto"
        );
    }

    #[test]
    fn test_status_response_shape() {
        // Verify the JSON structure matches what the frontend expects
        // (pure data-shape test — no AppState needed)
        let json = serde_json::json!({
            "data": {
                "active_sessions": 0,
                "shell": "none",
                "permission_level": "—",
                "permission_key": "semi_auto",
                "available_shells": [],
                "available_levels": [
                    { "key": "read_only", "label": "只读" },
                    { "key": "fully_manual", "label": "纯手动" },
                    { "key": "semi_auto", "label": "半自动" },
                    { "key": "fully_auto", "label": "全自动" },
                ]
            }
        });
        assert!(json["data"]["available_levels"].as_array().unwrap().len() == 4);
    }
}
