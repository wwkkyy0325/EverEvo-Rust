//! `SessionStore` — the server→agent session-state seam.
//!
//! During the P1.1 tool-ownership refactor, the server-layer tools
//! (`ask_user`, `problem_model`, sandbox shell, ...) moved INTO the agent
//! crate. Their session-scoped state (pending-ask map, confirmation map,
//! problem-model store, sandbox) lives in the HTTP/session layer, so the
//! agent tools depend on this trait and the server implements it at
//! registry-assembly time (architecture-restructure-plan.md P1.1, option B).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use everevo_core::problem_model::ProblemModel;
use everevo_core::session::{
    AskNotification, ConfirmationNotification, PendingAsk, PendingConfirmation,
};

/// Session-scoped state that agent tools need but that the server owns.
/// The server implements this for its per-session coordinator / AppState and
/// hands `Arc<dyn SessionStore>` to the tools when it assembles the registry.
pub trait SessionStore: Send + Sync {
    /// Pending `ask_user` questions — the reply endpoint fires the oneshot.
    fn ask_user_map(&self) -> Arc<RwLock<HashMap<Uuid, PendingAsk>>>;
    /// Channel to notify the SSE stream about a pending question.
    fn ask_notif_tx(&self) -> mpsc::UnboundedSender<AskNotification>;
    /// Auto mode (benchmark / FullyAuto parent): never block, reply immediately.
    fn auto_answer(&self) -> bool;

    /// Pending sandbox confirmations — the confirm endpoint resolves the oneshot.
    fn confirmations(&self) -> Arc<RwLock<HashMap<Uuid, PendingConfirmation>>>;
    /// Channel to notify the SSE stream about a pending confirmation.
    fn confirm_notif_tx(&self) -> mpsc::UnboundedSender<ConfirmationNotification>;
    /// Auto-confirm mode (sub-agent inheriting a FullyAuto parent).
    fn auto_confirm(&self) -> bool;

    /// Session-scoped problem-model store.
    fn problem_models(&self) -> Arc<RwLock<HashMap<Uuid, ProblemModel>>>;
}
