//! A2A Axum router — JSON-RPC 2.0 endpoint + Agent Card discovery.
//!
//! ## Routes
//!
//! | Method | Path | Handler |
//! |--------|------|---------|
//! | GET | `/.well-known/agent.json` | `serve_agent_card` |
//! | POST | `/a2a/rpc` | `jsonrpc_handler` |

use std::collections::HashMap;
use std::sync::Arc;

use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Extension;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::A2aError;
use crate::executor::A2aAgentExecutor;
use crate::types::{
    A2aTask, AgentCard, JsonRpcRequest, JsonRpcResponse, Part, TaskQueryParams, TaskSendParams,
    TaskState, TaskStatus,
};

// ── Shared State ──────────────────────────────────────────────────────────

/// Per-server A2A state — shared across all requests.
pub struct A2aState {
    /// The executor (production or stub).
    pub executor: Arc<dyn A2aAgentExecutor>,
    /// Pre-built AgentCard (rebuilt on config changes).
    pub agent_card: AgentCard,
    /// Active task registry: task_id → (status, cancel_token).
    pub tasks: Mutex<HashMap<String, TaskEntry>>,
    /// Default max turns for agent loop.
    pub max_turns: usize,
}

pub struct TaskEntry {
    pub task: A2aTask,
    pub cancel: CancellationToken,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl A2aState {
    pub fn new(
        executor: Arc<dyn A2aAgentExecutor>,
        agent_card: AgentCard,
        max_turns: usize,
    ) -> Self {
        Self {
            executor,
            agent_card,
            tasks: Mutex::new(HashMap::new()),
            max_turns,
        }
    }
}

// ── Router Construction ───────────────────────────────────────────────────

/// Build the A2A Axum router — uses `Extension<Arc<A2aState>>` for shared state
/// so it returns `Router<()>` that merges cleanly with any parent router.
pub fn a2a_router(state: Arc<A2aState>) -> axum::Router<()> {
    axum::Router::new()
        .route("/.well-known/agent.json", get(serve_agent_card))
        .route("/a2a/rpc", post(jsonrpc_handler))
        .route("/a2a/health", get(a2a_health))
        .layer(Extension(state))
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /.well-known/agent.json — serves the AgentCard for discovery.
async fn serve_agent_card(Extension(state): Extension<Arc<A2aState>>) -> Json<AgentCard> {
    Json(state.agent_card.clone())
}

/// GET /a2a/health — lightweight health check for the A2A gateway.
async fn a2a_health(Extension(state): Extension<Arc<A2aState>>) -> Json<serde_json::Value> {
    let task_count = state.tasks.lock().await.len();
    Json(serde_json::json!({
        "status": "ok",
        "protocol": "a2a/0.3.0",
        "active_tasks": task_count,
    }))
}

/// POST /a2a/rpc — JSON-RPC 2.0 dispatcher.
///
/// Routes by `method` field:
/// - `message/send` → execute task synchronously
/// - `tasks/get` → query task by ID
/// - `tasks/cancel` → cancel running task
async fn jsonrpc_handler(
    Extension(state): Extension<Arc<A2aState>>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let response = match request.method.as_str() {
        "message/send" => handle_message_send(Arc::clone(&state), &request).await,
        "tasks/get" => handle_tasks_get(&state, &request).await,
        "tasks/cancel" => handle_tasks_cancel(&state, &request).await,
        other => Err(A2aError::method_not_found(other)),
    };

    match response {
        Ok(result) => Json(JsonRpcResponse::success(
            request.id,
            serde_json::to_value(result).unwrap_or_default(),
        )),
        Err(e) => Json(e.to_jsonrpc_error(request.id)),
    }
}

// ── Method Handlers ───────────────────────────────────────────────────────

async fn handle_message_send(
    state: Arc<A2aState>,
    req: &JsonRpcRequest,
) -> Result<A2aTask, A2aError> {
    let params: TaskSendParams = serde_json::from_value(req.params.clone())
        .map_err(|e| A2aError::invalid_params(&e.to_string()))?;

    let task_id = uuid::Uuid::new_v4().to_string();
    let context_id = params
        .context_id
        .clone()
        .unwrap_or_else(|| format!("ctx-{}", &task_id[..8]));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let task = A2aTask::new(&task_id, &context_id);

    // Register task as "submitted"
    {
        let mut tasks = state.tasks.lock().await;
        tasks.insert(
            task_id.clone(),
            TaskEntry {
                task: task.clone(),
                cancel: cancel_clone,
                created_at: chrono::Utc::now(),
            },
        );
    }

    // Update to "working"
    {
        let mut tasks = state.tasks.lock().await;
        if let Some(entry) = tasks.get_mut(&task_id) {
            entry.task.status = TaskStatus::new(TaskState::Working);
        }
    }

    // Non-blocking mode: spawn background execution, return immediately.
    if params.blocking == Some(false) {
        let executor = Arc::clone(&state.executor);
        let state_bg = Arc::clone(&state);
        let tid = task_id.clone();
        let cid = context_id.clone();
        let msg = params.message.clone();
        tokio::spawn(async move {
            let result = executor.execute(&tid, &cid, &msg, cancel).await;
            let mut tasks = state_bg.tasks.lock().await;
            match result {
                Ok(completed) => {
                    if let Some(entry) = tasks.get_mut(&tid) {
                        entry.task = completed;
                    }
                }
                Err(e) => {
                    if let Some(entry) = tasks.get_mut(&tid) {
                        entry.task.status = TaskStatus::with_message(
                            TaskState::Failed,
                            crate::types::A2aMessage::agent(vec![Part::text(&e.message)]),
                        );
                    }
                }
            }
        });
        // Return current Working task — caller polls via tasks/get
        let tasks = state.tasks.lock().await;
        return Ok(tasks.get(&task_id).unwrap().task.clone());
    }

    // Blocking mode: await execution synchronously (existing behavior).
    let result = state
        .executor
        .execute(&task_id, &context_id, &params.message, cancel)
        .await;

    // Update final state
    let mut tasks = state.tasks.lock().await;
    match result {
        Ok(completed) => {
            if let Some(entry) = tasks.get_mut(&task_id) {
                entry.task = completed;
            }
            Ok(tasks.get(&task_id).unwrap().task.clone())
        }
        Err(e) => {
            if let Some(entry) = tasks.get_mut(&task_id) {
                entry.task.status = TaskStatus::with_message(
                    TaskState::Failed,
                    crate::types::A2aMessage::agent(vec![Part::text(&e.message)]),
                );
            }
            // Don't return the error — return the task with failed status
            Ok(tasks.get(&task_id).unwrap().task.clone())
        }
    }
}

async fn handle_tasks_get(
    state: &A2aState,
    req: &JsonRpcRequest,
) -> Result<A2aTask, A2aError> {
    let params: TaskQueryParams = serde_json::from_value(req.params.clone())
        .map_err(|e| A2aError::invalid_params(&e.to_string()))?;

    let tasks = state.tasks.lock().await;
    tasks
        .get(&params.id)
        .map(|entry| entry.task.clone())
        .ok_or_else(|| A2aError::task_not_found(&params.id))
}

async fn handle_tasks_cancel(
    state: &A2aState,
    req: &JsonRpcRequest,
) -> Result<A2aTask, A2aError> {
    let params: TaskQueryParams = serde_json::from_value(req.params.clone())
        .map_err(|e| A2aError::invalid_params(&e.to_string()))?;

    let mut tasks = state.tasks.lock().await;
    let entry = tasks
        .get(&params.id)
        .ok_or_else(|| A2aError::task_not_found(&params.id))?;

    // Check cancelable
    if !entry.task.status.state.is_cancelable() {
        let state_str = format!("{:?}", entry.task.status.state);
        return Err(A2aError::task_not_cancelable(&params.id, &state_str));
    }

    // Cancel and update
    entry.cancel.cancel();
    let mut task = entry.task.clone();
    task.status = TaskStatus::new(TaskState::Canceled);

    // Update in registry
    if let Some(e) = tasks.get_mut(&params.id) {
        e.task = task.clone();
    }

    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::AgentCardBuilder;
    use crate::executor::EchoExecutor;
    use crate::types::Part;

    fn test_state() -> Arc<A2aState> {
        let card = AgentCardBuilder::new("http://localhost:3000").build();
        Arc::new(A2aState::new(
            Arc::new(EchoExecutor),
            card,
            5,
        ))
    }

    #[tokio::test]
    async fn test_message_send_and_get() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "message/send".into(),
            params: serde_json::to_value(TaskSendParams {
                message: crate::types::A2aMessage::user(vec![Part::text("hello")]),
                context_id: None,
                session_id: None,
                blocking: Some(true),
                metadata: None,
            })
            .unwrap(),
            id: serde_json::Value::Number(1.into()),
        };

        let task = handle_message_send(Arc::clone(&state), &req).await.unwrap();
        assert_eq!(task.status.state, TaskState::Completed);
        assert!(task
            .status
            .message
            .unwrap()
            .text_content()
            .unwrap()
            .contains("Echo"));

        // Get the task
        let get_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "tasks/get".into(),
            params: serde_json::to_value(TaskQueryParams {
                id: task.id.clone(),
                history_length: None,
            })
            .unwrap(),
            id: serde_json::Value::Number(2.into()),
        };
        let fetched = handle_tasks_get(&state, &get_req).await.unwrap();
        assert_eq!(fetched.id, task.id);
    }

    #[tokio::test]
    async fn test_tasks_get_not_found() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "tasks/get".into(),
            params: serde_json::to_value(TaskQueryParams {
                id: "nonexistent".into(),
                history_length: None,
            })
            .unwrap(),
            id: serde_json::Value::Number(1.into()),
        };
        let err = handle_tasks_get(&state, &req).await.unwrap_err();
        assert_eq!(err.code, -32001);
    }
}
