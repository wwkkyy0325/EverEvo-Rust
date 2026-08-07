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

use axum::response::{IntoResponse, Json, Sse};
use axum::response::sse::Event;
use axum::routing::{get, post};
use axum::Extension;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::error::A2aError;
use crate::executor::A2aAgentExecutor;
use crate::types::{
    A2aTask, AgentCard, JsonRpcRequest, JsonRpcResponse, Part, TaskQueryParams,
    TaskSendParams, TaskState, TaskStatus,
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
/// - `tasks/list` → list tasks with optional status filter
/// - `tasks/sendSubscribe` → send message + subscribe to streaming updates
/// - `message/stream` → alias for tasks/sendSubscribe
async fn jsonrpc_handler(
    Extension(state): Extension<Arc<A2aState>>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    // SSE streaming — returned as Response<Body> to match the JSON fallback type
    if matches!(request.method.as_str(), "tasks/sendSubscribe" | "message/stream") {
        return handle_tasks_send_subscribe(Arc::clone(&state), &request)
            .await
            .into_response();
    }

    let task_to_json = |result: Result<A2aTask, A2aError>| -> Result<serde_json::Value, A2aError> {
        result.map(|task| serde_json::to_value(task).unwrap_or_default())
    };
    let response: Result<serde_json::Value, A2aError> = match request.method.as_str() {
        "message/send" => task_to_json(
            handle_message_send(Arc::clone(&state), &request).await
        ),
        "tasks/get" => task_to_json(
            handle_tasks_get(&state, &request).await
        ),
        "tasks/cancel" => task_to_json(
            handle_tasks_cancel(&state, &request).await
        ),
        "tasks/list" => handle_tasks_list(&state, &request).await,
        other => Err(A2aError::method_not_found(other)),
    };

    match response {
        Ok(result) => Json(JsonRpcResponse::success(request.id, result)).into_response(),
        Err(e) => Json(e.to_jsonrpc_error(request.id)).into_response(),
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

/// List tasks with optional status filter (A2A v1 feature).
async fn handle_tasks_list(
    state: &A2aState,
    req: &JsonRpcRequest,
) -> Result<serde_json::Value, A2aError> {
    // Parse optional filters
    let filter_state: Option<TaskState> = req
        .params
        .as_object()
        .and_then(|obj| obj.get("state"))
        .and_then(|s| s.as_str())
        .and_then(|s| serde_json::from_str(&format!("\"{s}\"")).ok());

    let tasks = state.tasks.lock().await;
    let filtered: Vec<&A2aTask> = tasks
        .values()
        .map(|e| &e.task)
        .filter(|t| {
            filter_state
                .as_ref()
                .map_or(true, |fs| t.status.state == *fs)
        })
        .collect();

    Ok(serde_json::json!({
        "tasks": filtered.iter().map(|t| serde_json::to_value(t).unwrap_or_default()).collect::<Vec<_>>(),
        "total": filtered.len(),
        "has_more": false,
    }))
}

/// Handle `tasks/sendSubscribe` — send a message and stream task lifecycle events via SSE.
/// Spawns the agent task in the background and returns an SSE stream of status updates.
async fn handle_tasks_send_subscribe(
    state: Arc<A2aState>,
    req: &JsonRpcRequest,
) -> impl IntoResponse {
    let params: TaskSendParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Json(JsonRpcResponse::error(
                serde_json::Value::Number(0.into()),
                -32602,
                &format!("Invalid params: {e}"),
            ))
            .into_response();
        }
    };

    let task_id = uuid::Uuid::new_v4().to_string();
    let context_id = params.context_id.clone().unwrap_or_else(|| format!("ctx-{}", &task_id[..8]));
    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    let executor = Arc::clone(&state.executor);
    let state_clone = Arc::clone(&state);

    tokio::spawn(async move {
        let cancel = CancellationToken::new();

        // Create task and register it
        let task = A2aTask::new(&task_id, &context_id);
        {
            let mut tasks = state_clone.tasks.lock().await;
            tasks.insert(
                task_id.clone(),
                TaskEntry {
                    task: task.clone(),
                    cancel: cancel.clone(),
                    created_at: chrono::Utc::now(),
                },
            );
        }

        // Emit initial task event
        let _ = tx
            .send(Ok(Event::default().event("task").data(
                serde_json::to_string(&serde_json::json!({
                    "id": task_id,
                    "context_id": context_id,
                    "state": "submitted",
                }))
                .unwrap_or_default(),
            )))
            .await;

        // Transition to working
        {
            let mut tasks = state_clone.tasks.lock().await;
            if let Some(entry) = tasks.get_mut(&task_id) {
                entry.task.status = TaskStatus::new(TaskState::Working);
            }
        }
        let _ = tx
            .send(Ok(Event::default().event("status-update").data(
                serde_json::json!({"task_id": task_id, "state": "working"}).to_string(),
            )))
            .await;

        // Build LLM messages from A2A message
        let _llm_messages: Vec<everevo_core::llm::LlmMessage> = params
            .message
            .parts
            .iter()
            .map(|part| match part {
                Part::Text { text } => match params.message.role.as_str() {
                    "user" => everevo_core::llm::LlmMessage::user(text),
                    _ => everevo_core::llm::LlmMessage::assistant(text),
                },
                Part::File { name, mime_type, uri, bytes: _ } => {
                    let desc = format!(
                        "[File: {} ({}) at {}]",
                        name.as_deref().unwrap_or("unnamed"),
                        mime_type.as_deref().unwrap_or("unknown"),
                        uri.as_deref().unwrap_or("inline"),
                    );
                    everevo_core::llm::LlmMessage::user(&desc)
                }
                Part::Data { data, .. } => {
                    let text = serde_json::to_string(data).unwrap_or_default();
                    everevo_core::llm::LlmMessage::user(&text)
                }
            })
            .collect();

        // Execute via the executor
        let result = executor
            .execute(&task_id, &context_id, &params.message, cancel.clone())
            .await;

        // Update the task in registry
        let final_state = match &result {
            Ok(task) => {
                let _ = tx
                    .send(Ok(Event::default().event("task").data(
                        serde_json::to_string(&task).unwrap_or_default(),
                    )))
                    .await;
                task.status.state
            }
            Err(_e) => TaskState::Failed,
        };
        {
            let mut tasks = state_clone.tasks.lock().await;
            if let Some(entry) = tasks.get_mut(&task_id) {
                entry.task = result.clone().unwrap_or_else(|_| {
                    let mut t = A2aTask::new(&task_id, &context_id);
                    t.status = TaskStatus::new(TaskState::Failed);
                    t
                });
            }
        }

        let _ = tx
            .send(Ok(Event::default().event("status-update").data(
                serde_json::json!({"task_id": task_id, "state": serde_json::to_string(&final_state).unwrap_or_default().trim_matches('"')}).to_string(),
            )))
            .await;
    });

    let stream = ReceiverStream::new(rx);
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
    .into_response()
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
