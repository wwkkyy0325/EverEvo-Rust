//! Per-session data-flow coordination hub.
//!
//! Every chat request creates one `SessionCoordinator`. It owns all channels,
//! shared state, and lifecycle primitives that need to be wired between
//! AgentLoop ↔ tools ↔ SSE stream for a single session.
//!
//! Previously these primitives were created ad-hoc in:
//! - chat.rs (SSE mpsc, confirmation mpsc, cancel token)
//! - orchestration/tools.rs (subagent mpsc, pending counter, backlog, etc.)
//!
//! Now they live in one struct — see the field, know the data flow.
//!
//! ## Data flow diagram
//!
//! ```text
//! SessionCoordinator (Clone, stored in AssembledTools)
//! ├─ tx_sse ──────────→ SSE stream → frontend
//! ├─ confirm_tx ──────→ (clone given to ShellTool)
//! ├─ pending (AtomicUsize) ←─ AgentLoop reads, all tools increment/decrement
//! ├─ backlog (Mutex<Vec>)  ←─ Task/Workflow/Team push, auto-continue drains
//! ├─ handles (Mutex<Vec>)  ←─ TaskTool creates, cancel API reads
//! ├─ statuses (Mutex<Vec>) ←─ All tools update, status API reads
//! ├─ compact_focus ──────── CompactTool writes → AgentLoop autocompact reads
//! └─ cancel (CancellationToken) ── SSE disconnect → agent run abort
//!
//! Receivers (returned separately — not Clone):
//!   sse_rx ── consumed by Axum SSE response
//!   confirm_rx ── consumed by SSE forward loop
//!   subagent_rx ── created inside TaskTool::take_receiver(), returned via AssembledTools
//! ```

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};

/// Receivers extracted from `SessionCoordinator::new()` — consumed by chat.rs.
pub struct SessionReceivers {
    pub sse_rx:
        mpsc::Receiver<Result<axum::response::sse::Event, std::convert::Infallible>>,
    pub confirm_rx:
        mpsc::UnboundedReceiver<crate::app_state::ConfirmationNotification>,
}

#[derive(Clone)]
pub struct SessionCoordinator {
    pub session_id: Uuid,

    // ── Channels (Sender side — Clone) ──────────────────
    /// SSE events to the frontend (bounded, backpressure at 256).
    pub tx_sse: mpsc::Sender<Result<axum::response::sse::Event, std::convert::Infallible>>,
    /// Shell confirmation notification sender (cloned to SandboxedShellTool).
    pub confirm_tx: mpsc::UnboundedSender<crate::app_state::ConfirmationNotification>,

    // ── Shared State (Arc/Mutex — Clone) ────────────────
    /// Running sub-agent count — AgentLoop blocks "Done" while >0.
    pub pending: Arc<std::sync::atomic::AtomicUsize>,
    /// Completed sub-agent results (id, description, result_text).
    /// Auto-continue loop drains this; Task/Workflow/Team push.
    pub backlog: Arc<Mutex<Vec<(String, String, String)>>>,
    /// Sub-agent lifecycle handles — cancel API uses.
    pub handles: Arc<Mutex<Vec<SubAgentHandle>>>,
    /// Sub-agent status snapshots — status API uses.
    pub statuses: Arc<Mutex<Vec<SubAgentStatus>>>,
    /// Compaction focus hint (CompactTool writes → autocompact reads and clears).
    pub compact_focus: Arc<Mutex<Option<String>>>,

    // ── Lifecycle (Clone) ───────────────────────────────
    /// Cancels the entire agent run on interrupt or SSE disconnect.
    pub cancel: CancellationToken,
}

impl SessionCoordinator {
    /// Create per-session channels and shared state.
    /// Returns (coordinator, sse_rx, confirm_rx).
    /// The receivers are consumed by chat.rs; the coordinator is passed through
    /// AssembledTools and back to chat.rs for the AgentLoop.
    pub fn new(
        session_id: Uuid,
    ) -> (Self, SessionReceivers) {
        let (tx_sse, sse_rx) = mpsc::channel(256);
        let (confirm_tx, confirm_rx) = mpsc::unbounded_channel();

        let coord = Self {
            session_id,
            tx_sse,
            confirm_tx,
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            backlog: Arc::new(Mutex::new(Vec::new())),
            handles: Arc::new(Mutex::new(Vec::new())),
            statuses: Arc::new(Mutex::new(Vec::new())),
            compact_focus: Arc::new(Mutex::new(None)),
            cancel: CancellationToken::new(),
        };

        (coord, SessionReceivers { sse_rx, confirm_rx })
    }
}
