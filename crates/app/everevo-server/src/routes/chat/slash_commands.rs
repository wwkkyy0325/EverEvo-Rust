//! Slash command handlers.

use std::convert::Infallible;

use axum::response::sse::Event;
use tokio::sync::mpsc;
use uuid::Uuid;
use super::helpers::resolve_permission;

use crate::app_state::AppState;

pub(super) async fn handle_character_command(
    state: &AppState,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    args: &str,
) -> String {
    let path = state
        .config.data_dir.join("memory").join("agent").join("character.json");
    if args.trim() == "sync" {
        let llm_opt = {
            let guard = state.llm.read().await;
            guard.values().find_map(|v| v.clone())
        };
        let result = match llm_opt {
            Some(llm) => everevo_agent::synthesize_character(&path, &*llm)
                .await
                .map(|r| r.note)
                .unwrap_or_else(|e| format!("Character sync failed: {e}")),
            None => "Character sync failed: no LLM provider configured.".into(),
        };
        let _ = tx
            .send(Ok(Event::default()
                .event("slash_command")
                .json_data(serde_json::json!({
                    "command": "character", "action": "sync", "result": result
                }))
                .unwrap_or_else(|_| Event::default().event("error"))))
            .await;
        result
    } else {
        let block = everevo_agent::build_character_block(&path)
            .unwrap_or_else(|| "Character not yet configured.".into());
        let _ = tx
            .send(Ok(Event::default()
                .event("slash_command")
                .json_data(serde_json::json!({
                    "command": "character", "action": "show"
                }))
                .unwrap_or_else(|_| Event::default().event("error"))))
            .await;
        block
    }
}

pub(super) async fn handle_plan_command(
    state: &AppState,
    session_id: Uuid,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    plan_task: &str,
) -> String {
    if plan_task == "cancel" || plan_task == "exit" {
        state.plan_mode_sessions.write().await.remove(&session_id);
        let _ = tx
            .send(Ok(Event::default()
                .event("plan_mode_exited")
                .json_data(serde_json::json!({"session_id": session_id.to_string()}))
                .unwrap_or_else(|_| Event::default().event("error"))))
            .await;
        tracing::info!(%session_id, "Plan mode cancelled by user");
        "Plan mode cancelled. Normal operations resumed.".to_string()
    } else {
        state
            .plan_mode_sessions.write().await
            .insert(session_id, "semi_auto".to_string());
        let _ = tx.send(Ok(Event::default()
            .event("plan_mode_entered")
            .json_data(serde_json::json!({"session_id": session_id.to_string(), "task": plan_task}))
            .unwrap_or_else(|_| Event::default().event("error")))).await;
        tracing::info!(%session_id, task = plan_task, "Plan mode entered via /plan command");
        if plan_task.is_empty() {
            "Plan mode entered via /plan. Explore the codebase, design an approach, \
             and write a plan. Write tools are blocked until the user approves."
                .to_string()
        } else {
            format!(
                "Plan mode entered for: {plan_task}\n\n\
                 Explore the codebase, design an approach, and write a plan. \
                 Write tools (shell, write_file, download) are blocked until approval."
            )
        }
    }
}

pub(super) async fn handle_workspace_command(
    state: &AppState,
    session_id: Uuid,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    path: &str,
) -> String {
    let path = path.trim();
    if path.is_empty() || path == "reset" {
        let _ = state.db.set_session_workspace(session_id, None).await;
        let _ = state
            .create_sandbox(
                session_id,
                resolve_permission(&state.config.default_permission_level),
                None,
            )
            .await;
        let _ = tx.send(Ok(Event::default()
            .event("workspace_changed")
            .json_data(serde_json::json!({"session_id": session_id.to_string(), "workspace_dir": null}))
            .unwrap_or_else(|_| Event::default().event("error")))).await;
        "Workspace reset to sandbox default. Shell commands now run in the isolated sandbox directory."
            .to_string()
    } else {
        let p = std::path::Path::new(path);
        if !p.is_dir() {
            format!("Error: '{path}' is not a valid directory.")
        } else {
            let ws = p.to_string_lossy().to_string();
            let _ = state.db.set_session_workspace(session_id, Some(&ws)).await;
            let _ = state
                .create_sandbox(
                    session_id,
                    resolve_permission(&state.config.default_permission_level),
                    Some(ws.clone()),
                )
                .await;
            let _ = tx.send(Ok(Event::default()
                .event("workspace_changed")
                .json_data(serde_json::json!({"session_id": session_id.to_string(), "workspace_dir": ws}))
                .unwrap_or_else(|_| Event::default().event("error")))).await;
            format!("Workspace set to: {path}\n\nAll shell commands and file operations will use this directory. Use `/workspace reset` to revert to sandbox default.")
        }
    }
}
