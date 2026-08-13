//! Single write-path coordinator for session content — P2 "write convergence"
//! (architecture-restructure-plan.md). All session-turn content (user / tool /
//! assistant) is persisted through this one call site, so the DB + dreaming
//! fan-out is centralized instead of scattered inline writes in the handler.
//!
//! Read authority is documented here too: the DB message table is the
//! conversation SOURCE for the LLM context; `RollingSummaryStage` /
//! autocompact are compression VIEWS injected into the messages by context
//! stages (never competing stores); the dreaming engine is the active-session
//! model feed; memory / workflows are long-term sinks for facts & procedures.

use std::sync::Arc;

use uuid::Uuid;

use everevo_core::EverEvoError;
use everevo_db::models::MessageRow;

use crate::app_state::AppState;

/// Per-session write coordinator.
pub struct SessionContent<'a> {
    state: &'a Arc<AppState>,
    session_id: Uuid,
}

impl<'a> SessionContent<'a> {
    pub fn new(state: &'a Arc<AppState>, session_id: Uuid) -> Self {
        Self { state, session_id }
    }

    /// Persist a user message to the DB (conversation history) AND feed it to
    /// the dreaming engine (active-session model). Single write path.
    pub async fn persist_user(&self, content: &str) -> Result<MessageRow, EverEvoError> {
        let row = MessageRow::new(self.session_id, "user", content, None, None, None);
        self.state.db.add_message(&row).await?;
        self.state.dreaming_engine.push_message(
            "user",
            content,
            &row.id.to_string(),
            &self.session_id.to_string(),
        );
        Ok(row)
    }

    /// Persist a turn row (assistant / tool stub / tool result) to the DB.
    pub async fn persist_turn(
        &self,
        role: &str,
        content: &str,
        tool_calls: Option<String>,
        tool_call_id: Option<String>,
        thinking: Option<String>,
    ) -> Result<MessageRow, EverEvoError> {
        let row = MessageRow::new(
            self.session_id,
            role,
            content,
            tool_calls,
            tool_call_id,
            thinking,
        );
        self.state.db.add_message(&row).await?;
        Ok(row)
    }
}
