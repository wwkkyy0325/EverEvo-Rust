//! The server's implementation of the agent [`SessionStore`] seam.
//!
//! During the P1.1 tool-ownership refactor the session-stateful tools
//! (`ask_user`, `problem_model`, sandbox shell) moved into the agent crate and
//! depend on `everevo_agent::tools::session_store::SessionStore`. This struct
//! bridges that trait to the server's `AppState` + per-session SSE channels.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use everevo_agent::tools::session_store::SessionStore;
use everevo_core::problem_model::ProblemModel;
use everevo_core::session::{
    AskNotification, ConfirmationNotification, PendingAsk, PendingConfirmation,
};

use crate::app_state::AppState;

/// Per-session bridge: AppState maps + the SSE notification channels.
pub struct ServerSessionStore {
    state: Arc<AppState>,
    session_id: Uuid,
    ask_notif_tx: mpsc::UnboundedSender<AskNotification>,
    confirm_notif_tx: mpsc::UnboundedSender<ConfirmationNotification>,
    auto_answer: bool,
    auto_confirm: bool,
}

impl ServerSessionStore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Arc<AppState>,
        session_id: Uuid,
        ask_notif_tx: mpsc::UnboundedSender<AskNotification>,
        confirm_notif_tx: mpsc::UnboundedSender<ConfirmationNotification>,
        auto_answer: bool,
        auto_confirm: bool,
    ) -> Self {
        Self {
            state,
            session_id,
            ask_notif_tx,
            confirm_notif_tx,
            auto_answer,
            auto_confirm,
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }
}

impl SessionStore for ServerSessionStore {
    fn ask_user_map(&self) -> Arc<RwLock<HashMap<Uuid, PendingAsk>>> {
        self.state.ask_user.clone()
    }
    fn ask_notif_tx(&self) -> mpsc::UnboundedSender<AskNotification> {
        self.ask_notif_tx.clone()
    }
    fn auto_answer(&self) -> bool {
        self.auto_answer
    }
    fn confirmations(&self) -> Arc<RwLock<HashMap<Uuid, PendingConfirmation>>> {
        self.state.confirmations.clone()
    }
    fn confirm_notif_tx(&self) -> mpsc::UnboundedSender<ConfirmationNotification> {
        self.confirm_notif_tx.clone()
    }
    fn auto_confirm(&self) -> bool {
        self.auto_confirm
    }
    fn problem_models(&self) -> Arc<RwLock<HashMap<Uuid, ProblemModel>>> {
        self.state.problem_models.clone()
    }
}
