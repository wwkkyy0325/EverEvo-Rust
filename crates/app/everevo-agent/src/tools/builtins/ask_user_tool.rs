//! AskUserTool — a blocking tool that asks the user a free-text question and
//! waits for their reply before the agent loop continues.
//!
//! Semantics follow Claude Code's `ask_user`:
//!   - No auto-timeout: the tool blocks indefinitely until the user replies,
//!     the run is cancelled (SSE disconnect / `/interrupt`), or the oneshot
//!     sender is dropped.
//!   - The frontend is notified via `AskNotification` over the SSE stream
//!     (`awaiting_user` event); the reply arrives at `POST /api/sessions/{id}/ask`.
//!   - Headless/auto mode (`auto_answer = true`, i.e. `EVEREVO_BENCHMARK` or a
//!     FullyAuto parent) short-circuits immediately with a fixed reply so the
//!     agent never deadlocks waiting for a human.
//!
//! Moved from the server crate during the P1.1 tool-ownership refactor: the
//! session-scoped state now comes from [`crate::tools::session_store::SessionStore`].

use std::sync::Arc;

use uuid::Uuid;

use everevo_core::session::{AskNotification, PendingAsk};

use crate::tools::session_store::SessionStore;

/// Fixed reply returned in auto mode — the agent proceeds on best judgment.
const ASK_USER_AUTO_REPLY: &str = "User not available (auto mode). Use best judgment and proceed.";

/// Tool that blocks the agent loop until the user answers a free-text question.
pub struct AskUserTool {
    pub session_id: Uuid,
    /// Session-scoped state provided by the server (pending-ask map, SSE notif
    /// channel, auto mode).
    pub store: Arc<dyn SessionStore>,
}

#[async_trait::async_trait]
impl everevo_core::tool::Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> &str {
        "Ask the user a question about their intent and wait (blocking) for their reply. \
         Use this when a decision is genuinely the user's to make and guessing would waste work. \
         Keep the question concise and single-focus."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user. Concise, single-focus."
                }
            },
            "required": ["question"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel {
        everevo_core::types::RiskLevel::Low
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        let question = params["question"].as_str().ok_or_else(|| {
            everevo_core::EverEvoError::InvalidInput("question is required".into())
        })?;
        let question = question.trim();

        // Auto mode (benchmark / fully_auto): never block a headless run.
        if self.store.auto_answer() {
            tracing::debug!(session_id = %self.session_id, "ask_user short-circuited (auto mode)");
            return Ok(everevo_core::tool::ToolOutput::text(ASK_USER_AUTO_REPLY));
        }

        let ask_user = self.store.ask_user_map();
        let notif_tx = self.store.ask_notif_tx();

        // Block: park a oneshot under the session id and notify the SSE stream.
        let (tx, rx) = tokio::sync::oneshot::channel();
        ask_user.write().await.insert(
            self.session_id,
            PendingAsk {
                question: question.to_string(),
                reply_tx: tx,
            },
        );
        let _ = notif_tx.send(AskNotification {
            session_id: self.session_id,
            question: question.to_string(),
        });
        tracing::info!(session_id = %self.session_id, question = %question, "Waiting for user reply...");

        // No auto-timeout (Claude Code style): only a user reply or a
        // cancelled run can unblock us.
        let reply = match cancel {
            Some(token) => {
                tokio::select! {
                    biased;
                    reply = rx => reply,
                    () = token.cancelled() => {
                        ask_user.write().await.remove(&self.session_id);
                        return Err(everevo_core::EverEvoError::Tool {
                            tool: "ask_user".into(),
                            message: "Run cancelled while waiting for user reply".into(),
                        });
                    }
                }
            }
            None => rx.await,
        };
        ask_user.write().await.remove(&self.session_id);

        match reply {
            Ok(answer) => {
                tracing::info!(session_id = %self.session_id, "Received user reply");
                Ok(everevo_core::tool::ToolOutput::text(answer))
            }
            Err(_recv) => Err(everevo_core::EverEvoError::Tool {
                tool: "ask_user".into(),
                message: "User disconnected while waiting for reply".into(),
            }),
        }
    }
}
