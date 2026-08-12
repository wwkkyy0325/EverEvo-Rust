# Fix Sub-agent Blocking, Permission Inheritance, and Lost Agent Tracking
> **状态**:✅ 已完成(归档)— 子代理阻塞/权限修复已落地

---


## Problem Summary

Three related issues reported 2026-07-21:

1. **Sub-agent confirmation in FullyAuto mode**: Main conversation = FullyAuto, but sub-agents
   still require user confirmation for shell commands.
2. **Third agent "失联"**: 3 sub-agents dispatched, 2 completed, 3rd disappeared — no
   telemetry, no activity records, main conversation hung.
3. **Main conversation blocked**: When waiting for sub-agents, UI frozen for 5 minutes.
   User cannot interject or cancel.

### Evidence from logs

```
2026-07-21T01:32:01.476182Z DEBUG everevo_agent::llm: SSE format auto-detected format="anthropic"
2026-07-21T01:32:11.780899Z  INFO everevo_agent::loop_: LLM says Done but sub-agents running — waiting pending=1
2026-07-21T01:37:11.782965Z  WARN everevo_agent::loop_: Timed out waiting for sub-agent results pending=1
```

---

## Root Cause Chain (all three issues are connected)

```
TaskTool::dispatch_one()                      [delegate.rs:55]
  ├── pending.fetch_add(1)                    ← counter incremented
  ├── tokio::spawn(async { ... })             ← JoinHandle DROPPED → orphan task
  │     └── spawn_single()                    [delegate.rs:111]
  │           ├── AgentLoop::new()            ← max_turns=0 = UNLIMITED
  │           ├── agent_loop.run(llm, tools, messages, None) ← confirmation=SKIPPED
  │           │     └── SandboxedShellTool.execute()
  │           │           └── if needs_confirmation: blocks on oneshot ← DEADLOCK
  │           └── .await loops forever if LLM doesn't emit Done ← "失联"
  └── main AgentLoop sees pending>0
        └── run_loop() blocks 300s on rx.recv()  [loop_.rs:321]
              └── No AgentEvents emitted → SSE stream silent → UI frozen
```

Three bugs, one chain:

1. **Unlimited turns** (delegate.rs:138) → sub-agent can loop forever
2. **No timeout** → spawn_single bypasses SubAgent.timeout
3. **JoinHandle dropped** (delegate.rs:65) → no cancellation possible
4. **Blocking wait** (loop_.rs:321) → main conversation frozen
5. **Permission deadlock** → sub-agent without user-accessible confirmation channel

---

## Concrete Implementation Plan

### Phase 1: Sub-agent Lifecycle Safety

#### 1.1 Add `tokio-util` dependency for `CancellationToken`

**File**: `Cargo.toml` (workspace root)
```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

**File**: `crates/everevo-agent/Cargo.toml`
```toml
tokio-util.workspace = true
```

**File**: `crates/everevo-server/Cargo.toml`
```toml
tokio-util.workspace = true
```

#### 1.2 Add `SubAgentHandle` and status tracking struct

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

Add struct:
```rust
use tokio_util::sync::CancellationToken;

/// Handle to a running sub-agent — enables monitoring and cancellation.
#[derive(Clone)]
pub struct SubAgentHandle {
    pub id: Uuid,
    pub description: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub cancel: CancellationToken,
}

/// Snapshot of sub-agent status for API reporting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubAgentStatus {
    pub id: Uuid,
    pub description: String,
    pub started_at: String,
    pub status: String, // "running" | "completed" | "failed" | "timeout" | "cancelled"
    pub elapsed_ms: u64,
}
```

#### 1.3 Update `TaskTool` to store handles and status registry

Add fields to `TaskTool`:
```rust
/// Running sub-agent handles for cancellation/monitoring.
pub handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
/// Completed sub-agent statuses (pruned on session end).
pub statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
```

Add methods:
```rust
impl TaskTool {
    /// Get status of all sub-agents (running + recently completed).
    pub fn get_statuses(&self) -> Vec<SubAgentStatus> { ... }

    /// Cancel a running sub-agent by ID.
    pub fn cancel(&self, id: Uuid) -> bool { ... }
}
```

#### 1.4 Update `dispatch_one` to use timeout + turn limit + cancellation

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

Key changes to `dispatch_one`:
```rust
fn dispatch_one(&self, desc: &str, stype: &str, max_turns: usize) {
    self.pending.fetch_add(1, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let handle = SubAgentHandle {
        id: Uuid::new_v4(),
        description: desc.to_string(),
        started_at: chrono::Utc::now(),
        cancel: cancel.clone(),
    };
    // Register handle
    self.handles.lock().unwrap().push(handle.clone());
    // Register running status
    self.statuses.lock().unwrap().push(SubAgentStatus {
        id: handle.id, description: desc.to_string(),
        started_at: handle.started_at.to_rfc3339(),
        status: "running".into(), elapsed_ms: 0,
    });

    // ... spawn with cancel token, timeout, max_turns
    tokio::spawn(async move {
        // Use tokio::select! for cancellation-aware execution
        let result = tokio::select! {
            _ = cancel.cancelled() => {
                // Cancelled
                update_status(&statuses, handle.id, "cancelled");
                pending.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            r = tokio::time::timeout(
                Duration::from_secs(SUBAAGENT_TIMEOUT_SECS),
                spawn_single(...)
            ) => {
                match r {
                    Ok(result) => { /* normal completion */ }
                    Err(_) => { /* timeout */ }
                }
            }
        };
        // ... send result, update status
    });
}
```

#### 1.5 Update `spawn_single` to accept max_turns and cancellation

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

```rust
async fn spawn_single(
    sandbox_root: &PathBuf,
    base_tools: &ToolRegistry,
    llm: Arc<HttpClient>,
    desc: &str,
    stype: &str,
    sub_ctx: &SubAgentContext,
    max_turns: usize,       // NEW: from config
    cancel: CancellationToken, // NEW: for interruption
) -> String {
```

Use `crate::AgentLoop::new().with_max_turns(max_turns)` instead of unlimited.

#### 1.6 Add cancel endpoint

**File**: `crates/everevo-server/src/routes/sandbox_routes.rs`

```rust
.route("/api/agent/tasks/{id}/cancel", post(cancel_subagent))
```

```rust
async fn cancel_subagent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    // Find the TaskTool's handles and cancel the matching one
    // ...
}
```

#### 1.7 Add status endpoint

```rust
.route("/api/agent/tasks", get(list_subagent_tasks))
```

```rust
async fn list_subagent_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionQuery>,
) -> Json<serde_json::Value> {
    // Return all sub-agent statuses for the session
}
```

---

### Phase 2: Non-blocking Agent Loop

#### 2.1 Add new `AgentEvent` variants

**File**: `crates/everevo-agent/src/loop_.rs`

```rust
pub enum AgentEvent {
    // ... existing variants ...
    /// Sub-agent was dispatched.
    SubAgentStarted { id: Uuid, description: String },
    /// Sub-agent completed.
    SubAgentResult { id: Uuid, description: String, result: String },
    /// LLM says Done but sub-agents are still running.
    WaitingForSubAgents { pending: usize },
}
```

#### 2.2 Remove blocking wait, replace with immediate return

**File**: `crates/everevo-agent/src/loop_.rs` lines 313-339

**BEFORE** (blocks 300s):
```rust
if tool_calls.is_empty() {
    let pending = pending_subagents.load(...);
    if pending > 0 {
        match tokio::time::timeout(Duration::from_secs(300), rx.recv()).await { ... }
    }
    let _ = tx.send(AgentEvent::Done { final_text }).await;
    return Ok(());
}
```

**AFTER** (returns immediately, sub-agent results arrive via channel):
```rust
if tool_calls.is_empty() {
    let pending = pending_subagents.load(Ordering::SeqCst);
    if pending > 0 {
        // Don't block — emit event and return.
        // Sub-agent results will be delivered via subagent_rx
        // and trigger a new agent turn in the chat route.
        let _ = tx.send(AgentEvent::WaitingForSubAgents { pending }).await;
        // Still drain any already-available results before returning
        if let Some(ref mut rx) = subagent_rx {
            while let Ok(result) = rx.try_recv() {
                let _ = tx.send(AgentEvent::SubAgentResult {
                    id: Uuid::nil(), description: String::new(), result,
                }).await;
            }
        }
        let final_text = current_text.clone();
        let _ = tx.send(AgentEvent::Done { final_text }).await;
        return Ok(());
    }
    let final_text = current_text.clone();
    let _ = tx.send(AgentEvent::Done { final_text }).await;
    return Ok(());
}
```

#### 2.3 SSE auto-continue when sub-agent results arrive

**File**: `crates/everevo-server/src/routes/chat.rs`

After the main agent loop emits `Done` with pending sub-agents:

1. Keep SSE connection open
2. Spawn a background task that waits for sub-agent results via a new channel
3. When a result arrives:
   - Inject `[SubAgent Result]` into messages
   - Start a new AgentLoop with the updated messages
   - Stream events on the same SSE channel
4. When pending reaches 0: emit `subagents_all_done` and close SSE

This requires a session-scoped sub-agent result channel that outlives a single chat request.

#### 2.4 Add interrupt endpoint

**File**: `crates/everevo-server/src/routes/chat.rs` (or `sandbox_routes.rs`)

```rust
.route("/api/chat/{id}/interrupt", post(interrupt_chat))
```

```rust
/// Cancel the current agent turn without killing sub-agents.
async fn interrupt_chat(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    if let Some(actor) = state.session_actors.read().await.get(&session_id) {
        actor.cancel();
        Json(serde_json::json!({ "data": { "interrupted": true } }))
    } else {
        Json(serde_json::json!({ "error": "No active agent run" }))
    }
}
```

#### 2.5 Add `SessionActor` to `AppState`

**File**: `crates/everevo-server/src/app_state.rs`

```rust
use tokio_util::sync::CancellationToken;

/// Per-session actor for agent run lifecycle management.
pub struct SessionActor {
    /// Cancel the current agent run.
    cancel_token: CancellationToken,
    /// Number of pending sub-agents.
    pending_subagents: Arc<AtomicUsize>,
    /// Channel to receive sub-agent results.
    subagent_result_tx: tokio::sync::mpsc::UnboundedSender<SubAgentResultMsg>,
    subagent_result_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<SubAgentResultMsg>>>>,
}

impl SessionActor {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            cancel_token: CancellationToken::new(),
            pending_subagents: Arc::new(AtomicUsize::new(0)),
            subagent_result_tx: tx,
            subagent_result_rx: Arc::new(Mutex::new(Some(rx))),
        }
    }

    pub fn cancel(&self) { self.cancel_token.cancel(); }
    pub fn child_token(&self) -> CancellationToken { self.cancel_token.child_token(); }
}
```

Add to `AppState`:
```rust
pub session_actors: RwLock<HashMap<uuid::Uuid, SessionActor>>,
```

---

### Phase 3: Permission Inheritance

#### 3.1 Add `permission_level` to `SubAgentContext`

**File**: `crates/everevo-agent/src/subagent_context.rs`

```rust
pub struct SubAgentContext {
    // ... existing fields ...
    /// Parent session's permission level for inheritance.
    pub permission_level: Option<String>, // "fully_auto" | "semi_auto" | ...
}
```

Add to `build_system_prompt()`:
```rust
if let Some(ref level) = self.permission_level {
    prompt.push_str("## Permission Level\n");
    prompt.push_str(&format!("Parent session permission: {level}\n"));
    if level == "fully_auto" || level == "全自动" {
        prompt.push_str("Your shell commands are auto-approved. Do NOT ask for confirmation.\n");
    }
    prompt.push_str("\n");
}
```

#### 3.2 Pass permission through in `handle_chat`

**File**: `crates/everevo-server/src/routes/chat.rs`

When building `sub_ctx`:
```rust
let permission_level = state.sandboxes.read().await
    .get(&session_id)
    .map(|sb| sb.permission_level().label().to_string());

let sub_ctx = assemble_subagent_context(
    ...
).await;
sub_ctx.permission_level = permission_level;
```

#### 3.3 Sub-agent shell tool in FullyAuto: pre-confirm

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

In `spawn_single()`, check permission level before executing shell commands:
```rust
let is_fully_auto = sub_ctx.permission_level.as_deref() == Some("全自动")
    || sub_ctx.permission_level.as_deref() == Some("fully_auto");
```

When `is_fully_auto`:
- The `SandboxedShellTool` already handles this correctly (TieredSandbox returns Allow for non-admin commands)
- BUT for sub-agents with admin commands: instead of deadlocking on confirmation oneshot,
  the sub-agent should receive a clear error. This is handled by the shell tool naturally
  — if confirmation is needed and there's no SSE listener to handle it, the timeout
  will eventually trigger (or we add a shorter timeout for the oneshot in sub-agent context).

#### 3.4 Fail-fast for admin commands in sub-agents

**File**: `crates/everevo-server/src/routes/chat.rs` — `SandboxedShellTool::execute()`

Add a check: if running in sub-agent context (no notif_rx listener), admin commands
that require confirmation should fail-fast with a clear message instead of blocking
on the oneshot:

```rust
// In SandboxedShellTool::execute(), after checking needs_confirmation:
if result.needs_confirmation && is_subagent_context {
    return Ok(ToolOutput {
        content: format!(
            "Admin command '{}' requires user confirmation. \
             Sub-agents cannot request confirmation. Use a non-admin alternative.",
            command
        ),
        is_error: true,
    });
}
```

This requires a way to detect sub-agent context. Options:
- Add a field to `SandboxedShellTool`: `is_subagent: bool`
- Or check if `notif_tx` is closed (the sub-agent has no SSE listener)

---

### Implementation Order

| Step | What | Files | Risk |
|------|------|-------|------|
| 1 | Add `tokio-util` dep | workspace Cargo.toml, everevo-agent, everevo-server | None |
| 2 | Add `SubAgentHandle`, `SubAgentStatus` structs | delegate.rs | None |
| 3 | Add `handles`, `statuses` to `TaskTool` | delegate.rs | None |
| 4 | Update `dispatch_one`: timeout + max_turns + cancel + status tracking | delegate.rs | Med |
| 5 | Update `spawn_single`: accept max_turns, cancel token | delegate.rs | Med |
| 6 | Add `WaitingForSubAgents`, `SubAgentResult` events | loop_.rs | Med |
| 7 | **Remove blocking wait** in `run_loop()` | loop_.rs | **High** |
| 8 | Add `SessionActor` struct | app_state.rs | Med |
| 9 | Update `handle_chat` for auto-continue, session actor | chat.rs | **High** |
| 10 | Add `permission_level` to `SubAgentContext` | subagent_context.rs | Low |
| 11 | Pass permission through in chat route | chat.rs | Low |
| 12 | Add cancel + status endpoints | sandbox_routes.rs | Low |
| 13 | Add interrupt endpoint | chat.rs or sandbox_routes.rs | Low |
| 14 | `cargo test --workspace && cargo clippy --workspace` | — | — |

---

## Verification

1. **Phase 1**: Sub-agent with max_turns=5 and timeout=60s completes or times out.
   Cancelled sub-agent stops. GET /api/agent/tasks shows running sub-agents.

2. **Phase 2**: SSE emits "done" immediately even with pending sub-agents.
   Sub-agent result triggers auto-continue on same SSE connection.
   POST /api/chat/{id}/interrupt cancels current turn.

3. **Phase 3**: FullyAuto session → sub-agent shell commands auto-execute.
   Admin commands (sudo) fail-fast with clear error.