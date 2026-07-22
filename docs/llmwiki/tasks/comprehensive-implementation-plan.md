# Comprehensive Implementation Plan — EverEvo-Rust Audit Fixes

**Created**: 2026-07-21 | **Status**: In Progress

---

## Research Sources

- [MCP Resumable Streams](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/899) — transport-agnostic stream resume
- [A2A Protocol Streaming](https://a2a-protocol.org/v1.0.0/topics/streaming-and-async/) — SSE + resubscription + push notifications
- [Claude Code interrupt UX (#67191)](https://github.com/anthropics/claude-code/issues/67191) — queue vs interrupt debate
- [Claude Code compose-while-working (#62856)](https://github.com/anthropics/claude-code/issues/62856) — Codex-style deferred interrupt
- [Rust tokio JoinHandle best practices (2025)](https://users.rust-lang.org/t/tokios-spawn-tasks-and-join-handles/131438)
- [Rust graceful shutdown with CancellationToken](https://users.rust-lang.org/t/gracefully-terminating-tasks/126349)
- [Claude Code UX reverse-engineering](https://dev.to/ji_ai/i-reverse-engineered-claude-codes-80k-lines-of-source-heres-what-i-found-i13)

---

## Architecture Decisions

### A. SSE Auto-Resume Model

**Decision**: Keep SSE connection open during sub-agent wait, stream results inline.

```
POST /api/chat → SSE stream:
  token... token... token...
  tool_start → tool_end...
  waiting {pending: 3}        ← LLM says Done, but sub-agents running
  [KEEP SSE OPEN]             ← heartbeat every 30s
  subagent_result {id, desc}  ← sub-agent #1 completes
  [BACKEND AUTO-TRIGGERS]     ← inject result, restart AgentLoop
  token... token...           ← LLM responds to sub-agent result
  subagent_result {id, desc}  ← sub-agent #2 completes
  [AUTO-TRIGGER AGAIN]
  token... token...
  subagent_result {id, desc}  ← sub-agent #3 completes
  [AUTO-TRIGGER AGAIN]
  token... token...
  done {session_id, msg_id}   ← all sub-agents complete + LLM final response
```

**Why not MCP resumable streams**: Overkill for single-connection SSE. Our SSE connection stays alive; if it drops, the frontend polls GET /api/agent/tasks and reconnects.

### B. Tokio Task Management

**Decision**: Use `JoinSet` for sub-agent tasks, `CancellationToken` for graceful shutdown.

- `TaskTool` stores `JoinSet<()>` instead of `Vec<JoinHandle<()>>`
- Sub-agent dispatch: `join_set.spawn(async { ... })`
- Shutdown: iterate `join_set.join_next()` with timeout
- All `lock().unwrap()` → `lock().unwrap_or_else(|e| e.into_inner())` (poison recovery)

### C. Frontend Interrupt Model

**Decision**: Claude Code hybrid — Enter queues, Esc interrupts.

| State | Input Box | Enter Key | Esc Key |
|-------|-----------|-----------|---------|
| Idle | Enabled | Send | — |
| Streaming (main agent) | **Enabled** | Queue message | Interrupt & send queued |
| Waiting for sub-agents | **Enabled** | Queue message | Interrupt (cancel all sub-agents) |
| Error | Disabled | — | — |

---

## Implementation Batches

### Batch 1 — SSE Auto-Continue + Frontend Events (P0)

**Goal**: Sub-agent lifecycle is fully observable and self-driving. The conversation auto-resumes when sub-agent results arrive.

#### 1.1 Backend: Fix the Done-while-pending logic

**File**: `crates/everevo-agent/src/loop_.rs`

- Remove the `AgentEvent::Done` emission when pending > 0
- Only emit `WaitingForSubAgents { pending }` — do NOT emit Done
- The chat route decides when to emit Done (after all sub-agents complete)

```rust
// BEFORE (broken):
if pending > 0 {
    let _ = tx.send(AgentEvent::WaitingForSubAgents { pending }).await;
    let _ = tx.send(AgentEvent::Done { final_text }).await;  // ← WRONG: not done!
    return Ok(());
}

// AFTER (fixed):
if pending > 0 {
    let _ = tx.send(AgentEvent::WaitingForSubAgents { pending }).await;
    // Save partial text for later synthesis
    let _ = tx.send(AgentEvent::TextDelta(current_text.clone())).await;
    return Ok(());  // Don't emit Done — chat route handles it
}
```

#### 1.2 Backend: Auto-continue in chat route

**File**: `crates/everevo-server/src/routes/chat.rs`

After the main agent loop ends (agent_rx channel closes):

```rust
// After the main SSE loop breaks (agent_rx closed):
// Check if sub-agents are still pending
let pending = pending_subagents.load(Ordering::SeqCst);
if pending > 0 {
    // Keep SSE open, wait for sub-agent results
    // For each result that arrives:
    //   1. Send subagent_result SSE event
    //   2. Inject result into messages
    //   3. Spawn a new AgentLoop run
    //   4. Stream its events on the same tx
    loop {
        // Wait for next sub-agent result (with heartbeat every 30s)
        match tokio::time::timeout(Duration::from_secs(30), subagent_rx.recv()).await {
            Ok(Some(result)) => {
                // Send subagent_result event
                let _ = tx.send(Ok(Event::default()
                    .event("subagent_result")
                    .data(result.clone()))).await;
                // Inject and restart agent loop
                messages.push(LlmMessage::user(&format!("[SubAgent Result]\n{result}")));
                // Run a new AgentLoop (reuse tools, confirmation)
                let mut agent_rx2 = agent.run(
                    Arc::clone(&client), Arc::clone(&tools),
                    messages.clone(), None,
                ).await;
                // Stream events from this run
                while let Some(event) = agent_rx2.recv().await {
                    // ... forward events to SSE tx ...
                    if matches!(event, AgentEvent::Done { .. }) { break; }
                }
                // Check pending again
                if pending_subagents.load(Ordering::SeqCst) == 0 {
                    break; // All done
                }
            }
            Ok(None) => break, // Channel closed
            Err(_) => {
                // Heartbeat: send keepalive comment
                let _ = tx.send(Ok(Event::default()
                    .comment("heartbeat")).await);
            }
        }
    }
}
// Now emit final Done
let _ = tx.send(Ok(Event::default()
    .event("done")
    .data(serde_json::json!({"session_id": session_id, "message_id": assistant_id}).to_string())
)).await;
```

#### 1.3 Frontend: Handle new SSE events

**File**: `frontend/src/store.ts`

Add handlers for:
- `subagent_started` → add to `subagentTasks[]` state
- `subagent_result` → update task status, display inline
- `waiting` → show "Waiting for N sub-agents..." indicator
- heartbeat comments → ignored (SSE spec)

```typescript
// New state fields
interface ChatState {
  // ... existing ...
  subagentTasks: SubAgentTask[];  // running + recently completed
  waitingForSubagents: number;     // pending count, 0 = not waiting
}

interface SubAgentTask {
  id: string;
  description: string;
  status: 'running' | 'completed' | 'failed' | 'timeout' | 'cancelled';
  result?: string;
}

// In sendMessage SSE parser:
if (currentEvent === 'subagent_started') {
  const sa = JSON.parse(data);
  set((s) => ({
    subagentTasks: [...s.subagentTasks, {
      id: sa.id, description: sa.description, status: 'running'
    }]
  }));
}
if (currentEvent === 'subagent_result') {
  const sa = JSON.parse(data);
  set((s) => ({
    subagentTasks: s.subagentTasks.map(t =>
      t.id === sa.id ? { ...t, status: 'completed', result: sa.result } : t
    )
  }));
}
if (currentEvent === 'waiting') {
  const w = JSON.parse(data);
  set({ waitingForSubagents: w.pending });
}
```

#### 1.4 Frontend: Unlock input during streaming

**File**: `frontend/src/store.ts`

Change `sendMessage` to allow queuing:

```typescript
sendMessage: async (text: string) => {
  const { streaming, waitingForSubagents } = get();
  
  if (streaming) {
    // Queue the message — will be sent after current turn completes
    set((s) => ({ queuedMessage: text }));
    // If user explicitly wants to interrupt (via interrupt button):
    // POST /api/chat/{id}/interrupt → then send the queued message
    return;
  }
  // ... normal send flow ...
}
```

**File**: `frontend/src/components/ChatView.tsx`

- Remove `disabled={streaming}` from input
- Add interrupt button (visible only when streaming)
- Add "Waiting for N sub-agents..." indicator

#### 1.5 Frontend: Sub-agent status indicator

**File**: `frontend/src/components/ChatView.tsx` (new section after tool calls)

```tsx
{waitingForSubagents > 0 && (
  <div className="flex items-center gap-2 text-sm text-yellow-400 bg-yellow-400/10 rounded-lg px-3 py-2">
    <Loader2 className="animate-spin h-4 w-4" />
    Waiting for {waitingForSubagents} sub-agent{waitingForSubagents > 1 ? 's' : ''}...
  </div>
)}
{subagentTasks.filter(t => t.status === 'completed').map(t => (
  <SubAgentResultCard key={t.id} task={t} />
))}
```

---

### Batch 2 — Critical Safety Fixes (P0)

**Goal**: Eliminate crash-risk unwraps and unsafe code gaps.

#### 2.1 Fix classifier.rs unwrap

**File**: `crates/everevo-domain/src/classifier.rs:55`

```rust
// BEFORE:
domain_id: best_id.unwrap(),

// AFTER:
domain_id: match best_id {
    Some(id) => id,
    None => {
        tracing::warn!("High similarity but no domain ID — registry may be empty");
        return None;
    }
},
```

#### 2.2 Fix job_object.rs unsafe annotations

**File**: `crates/everevo-sandbox/src/job_object.rs`

- Replace `#![allow(unsafe_code)]` with per-block `#[allow(unsafe_code)]`
- Add `// SAFETY:` comments to each unsafe block
- Document invariants for `unsafe impl Send/Sync`

#### 2.3 Fix all bare unwraps

| File:Line | Fix |
|-----------|-----|
| chat.rs:272 | `task_tool.subagent_ctx.write().map_err(|e| format!("lock: {e}"))?` |
| app_state.rs:164 | `.unwrap_or_else(|e| { tracing::error!("SkillRegistry fallback: {e}"); SkillRegistry::empty() })` |
| pipeline.rs:351,463 | `.ok_or_else(|| EverEvoError::Internal("manifest/tracker mismatch".into()))?` |

#### 2.4 Fix all lock().unwrap() → poison recovery

Pattern: `foo.lock().unwrap()` → `foo.lock().unwrap_or_else(|e| e.into_inner())`

Files: delegate.rs (8 sites), loop_.rs (2 sites), others.

---

### Batch 3 — Config + Sandbox (P1)

**Goal**: Wire config values, isolate sub-agent sandboxes.

#### 3.1 Read config values in delegate.rs

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

Remove hardcoded `const DEFAULT_*`, read from `AppConfig`:

```rust
// Add config fields to TaskTool
pub struct TaskTool {
    // ... existing ...
    subagent_max_turns: usize,
    subagent_timeout_secs: u64,
}

// In dispatch_one, use self.subagent_max_turns / self.subagent_timeout_secs
```

**File**: `crates/everevo-server/src/routes/chat.rs`

Pass config values when creating TaskTool:

```rust
let task_tool = TaskTool::new(...)
    .with_subagent_max_turns(100)    // from config or default
    .with_subagent_timeout(600);     // from config or default
```

Actually, simpler: read from `config_center` or just pass from AppConfig at creation.

#### 3.2 Sub-agent sandbox isolation

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

In `spawn_single()`, create a per-subagent work directory:

```rust
let subagent_work_dir = sandbox_root
    .join("subagents")
    .join(subagent_id.to_string())
    .join("work");
std::fs::create_dir_all(&subagent_work_dir).ok();

// Use a custom SandboxProvider that wraps the parent's with a different work_dir
// OR: just set working_dir in ExecutionConfig (simpler)
```

Actually, the shell tool already forces work_dir in its ExecutionConfig. The fix is simpler: create a separate SandboxedShellTool for sub-agents that points to the subagent's work_dir.

**File**: `crates/everevo-server/src/routes/chat.rs`

When creating `base_for_task` in FullyAuto mode, create a shell tool with subagent-scoped work_dir. But the work_dir needs to be dynamic (per subagent). Option: pass the sandbox provider directly and let `spawn_single` create the shell tool with the right work_dir.

Simplest approach:

```rust
// In spawn_single, instead of using base_tools' shell tool,
// create a fresh TieredSandbox or SessionSandbox for the subagent
// and wrap it in a minimal shell tool without the confirmation gate.
```

---

### Batch 4 — Task Lifecycle Safety (P1)

**Goal**: Proper JoinSet tracking, graceful shutdown.

#### 4.1 Replace detached JoinHandles with JoinSet

**File**: `crates/everevo-agent/src/tools/builtins/delegate.rs`

```rust
use tokio::task::JoinSet;

pub struct TaskTool {
    // ... existing ...
    /// Sub-agent task handles for lifecycle management.
    pub tasks: Arc<std::sync::Mutex<JoinSet<()>>>,
}
```

In `dispatch_one`:
```rust
self.tasks.lock().unwrap_or_else(|e| e.into_inner())
    .spawn(async move { /* sub-agent work */ });
```

In a cleanup/drop handler:
```rust
// Drain remaining tasks on shutdown
let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
while let Some(result) = tasks.join_next().await {
    if let Err(e) = result {
        tracing::warn!("Sub-agent task error: {e}");
    }
}
```

#### 4.2 Graceful shutdown hook

**File**: `crates/everevo-server/src/app_state.rs`

```rust
impl AppState {
    pub async fn shutdown(&self) {
        // Cancel all active agent runs
        for (_sid, token) in self.session_actors.read().await.iter() {
            token.cancel();
        }
        // Give tasks 5s to clean up
        tokio::time::sleep(Duration::from_secs(5)).await;
        // Destroy sandboxes
        self.destroy_all_sandboxes().await;
    }
}
```

---

### Batch 5 — Dead Code Cleanup (P2)

**Goal**: Remove or consolidate unused code.

#### 5.1 Merge orchestration.rs into delegate.rs

- Keep `TaskType` enum (move to delegate.rs)
- Remove `AgentPool`, `SupervisorAgent`, `SubAgent`, `TaskDecomposer`
- The `/api/agent/delegate` endpoint can use a simplified version or be removed
- Keep `AgentContext` as it's referenced in design docs
- ~400 lines removed

#### 5.2 Remove unused exports

**File**: `crates/everevo-agent/src/lib.rs`

- Remove: `pub use sandbox` (no external consumers)
- Keep: `AgentKnowledgeGraph`, `LlmwikiManager` (may be used by future CLI tools)

---

### Batch 6 — Frontend Polish (P2)

**Goal**: Markdown rendering, sub-agent UI, session management.

#### 6.1 Enable react-markdown

**File**: `frontend/src/components/ChatView.tsx`

```tsx
import ReactMarkdown from 'react-markdown';
import rehypeHighlight from 'rehype-highlight';

// Replace <p className="whitespace-pre-wrap">{msg.content}</p>
<ReactMarkdown rehypePlugins={[rehypeHighlight]}>
  {msg.content}
</ReactMarkdown>
```

#### 6.2 Add interrupt button

```tsx
{streaming && (
  <button onClick={handleInterrupt}
    className="px-3 py-1 bg-red-500/20 text-red-400 rounded-lg text-sm">
    ⏹ Stop
  </button>
)}
```

#### 6.3 Add sub-agent result cards

New component: `SubAgentCard.tsx` — collapsible card showing sub-agent description, duration, result.

---

## Execution Order (Dependency Graph)

```
B2 (safety fixes) ── independent, can run anytime
     │
B1 (SSE auto-continue) ── depends on B2 (needs poison-safe locks)
     │
B3 (config + sandbox) ── depends on B1 (needs new delegate.rs structure)
     │
B4 (task lifecycle) ── depends on B3 (needs JoinSet from refactored delegate.rs)
     │
B5 (dead code) ── depends on B4 (after delegate.rs is stable)
     │
B6 (frontend polish) ── depends on B1 (needs new SSE events flowing)
```

**Recommended**: Start B2 + B1 together (they touch different files), then B3 → B4 → B5 → B6.

---

## Verification Checklist

After each batch:

- [ ] `cargo check` — zero errors
- [ ] `cargo clippy --workspace` — zero NEW warnings
- [ ] `cargo test --workspace` — zero NEW failures
- [ ] Manual: start server, send message that spawns sub-agents, verify:
  - [ ] Sub-agent start/result events appear in frontend
  - [ ] SSE auto-continues when sub-agent completes
  - [ ] Esc interrupts current turn
  - [ ] Permission change mid-stream doesn't deadlock
  - [ ] Sub-agent timeout actually terminates runaway sub-agents
