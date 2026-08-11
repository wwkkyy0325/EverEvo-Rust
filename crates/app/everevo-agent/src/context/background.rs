//! Layer-1 background maintenance (spec rule 5/6/7 + decision 1).
//!
//! Triggered at turn boundaries when the conversation is past the soft
//! threshold (~70% of the context budget). The task:
//!
//! 1. Reads this session's durable rolling summary + watermark from the DB.
//! 2. Fetches only the messages newer than the watermark.
//! 3. Runs `maintain_rolling_summary` (budget-aware chunking, never
//!    re-summarizes the old summary — rule 1).
//! 4. Writes the merged summary + advanced watermark back to the DB.
//!
//! ## Non-blocking
//!
//! The task only touches persisted state (sessions table); it never mutates
//! the in-flight `messages` vec, so there is no race with the main loop. The
//! main loop spawns it and never awaits it. `in_flight` is a shared flag so
//! only one maintenance task runs per session at a time.
//!
//! Fact extraction is intentionally NOT duplicated here — the existing
//! `DreamingEngine` already runs background per-turn fact extraction, which
//! is the second half of the spec's Layer-1.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use everevo_core::llm::LlmProvider;
use everevo_core::EverEvoError;
use everevo_db::Database;
use uuid::Uuid;

use crate::context::rolling_summary::maintain_rolling_summary;

/// Context needed for per-turn background rolling-summary maintenance.
#[derive(Clone)]
pub struct BackgroundMaintenance {
    pub db: Database,
    pub session_id: Uuid,
    /// Model used for compaction — the configured compact model, else the main
    /// model (decision 1: "有哪个用哪个").
    pub llm: Arc<dyn LlmProvider>,
    /// Compaction model's context window (tokens), for budget-aware chunking.
    pub ctx_window: Option<u32>,
    /// Shared in-flight guard — set true while a maintenance task runs so the
    /// turn-boundary trigger skips when a task is already active.
    pub in_flight: Arc<AtomicBool>,
}

impl BackgroundMaintenance {
    /// Run one incremental rolling-summary pass. Best-effort: errors are
    /// logged and swallowed (the task must never block or crash the loop).
    pub async fn maintain(&self) -> Result<(), EverEvoError> {
        let (old_summary, watermark) = self.db.get_session_context(self.session_id).await?;

        // Watermark → timestamp; epoch if none (summarize everything).
        let after = match &watermark {
            Some(wm) => self
                .db
                .get_message_created_at(self.session_id, wm)
                .await?
                .unwrap_or_else(epoch_start),
            None => epoch_start(),
        };

        let new_messages = self.db.get_messages_after(self.session_id, after).await?;
        if new_messages.is_empty() {
            return Ok(()); // nothing new since the last pass
        }

        let result = maintain_rolling_summary(
            &*self.llm,
            &new_messages,
            old_summary.as_deref(),
            self.ctx_window,
        )
        .await;

        match &result.watermark_message_id {
            Some(wm) => {
                self.db
                    .update_session_context(self.session_id, Some(&result.summary), Some(wm))
                    .await?;
                tracing::info!(
                    session = %self.session_id,
                    summarized = new_messages.len(),
                    watermark = %wm,
                    "Background rolling summary updated"
                );
                Ok(())
            }
            None => Ok(()), // no new messages summarized; leave summary unchanged
        }
    }
}

fn epoch_start() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch is always valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use crate::context::rolling_summary::SUMMARY_CAP_TOKENS;
    use crate::llm::mock::MockLlmProvider;
    use everevo_core::llm::{FinishReason, LlmResponse};
    use everevo_db::models::MessageRow;

    /// Build a MessageRow with an explicit created_at so watermark comparisons
    /// (strict `created_at >`) are deterministic across rapid inserts.
    fn row_at(
        session_id: Uuid,
        base: chrono::DateTime<chrono::Utc>,
        index: i64,
        role: &str,
        content: &str,
    ) -> MessageRow {
        let mut r = MessageRow::new(session_id, role, content, None, None, None);
        r.created_at = base + chrono::Duration::milliseconds(index);
        r
    }

    fn epoch() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch valid")
    }

    /// One maintenance pass summarizes the post-watermark messages and persists
    /// the summary + advanced watermark. A second pass with no new messages is
    /// a no-op (no LLM call, no regression).
    #[tokio::test]
    async fn maintain_writes_summary_and_advances_watermark() {
        let db = Database::connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory DB");
        let session = db.create_session("test").await.unwrap();
        db.add_message(&MessageRow::new(
            session.id,
            "user",
            "First fact: Atlas migration in September.",
            None,
            None,
            None,
        ))
        .await
        .unwrap();
        db.add_message(&MessageRow::new(
            session.id,
            "assistant",
            "Second fact: pricing 42 USD per seat.",
            None,
            None,
            None,
        ))
        .await
        .unwrap();

        let mock = Arc::new(MockLlmProvider::new().with_text("Concise summary."));
        let bg = BackgroundMaintenance {
            db,
            session_id: session.id,
            llm: mock.clone() as Arc<dyn LlmProvider>,
            ctx_window: Some(32_768),
            in_flight: Arc::new(AtomicBool::new(false)),
        };
        bg.maintain().await.expect("maintenance succeeds");

        let (summary, watermark) = bg.db.get_session_context(session.id).await.unwrap();
        assert!(summary.is_some(), "summary persisted");
        assert!(watermark.is_some(), "watermark advanced");

        // Second pass: nothing new → no-op, existing state preserved.
        bg.maintain().await.expect("idempotent second pass");
        let (summary2, watermark2) = bg.db.get_session_context(session.id).await.unwrap();
        assert_eq!(summary2, summary);
        assert_eq!(watermark2, watermark);
        assert_eq!(mock.call_count(), 1, "only the first pass calls the LLM");
    }

    /// Spec deliverable 8 (acceptance): a ~40-request conversation, one pass per
    /// request (exactly how the server runs — every user message is a fresh
    /// `run_loop`, so maintenance happens per request against persisted state).
    ///
    /// Verifies:
    ///   (a) the rolling summary stays bounded (≤ cap) — no unbounded growth;
    ///   (b) early key facts remain recallable from the durable summary;
    ///   (c) nothing is ever evicted from the DB (messages are append-only);
    ///   (d) the current turn is fully pending before each maintenance pass.
    #[tokio::test]
    async fn multi_request_watermark_stays_bounded_and_recallable() {
        let db = Database::connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory DB");
        let session = db.create_session("acceptance").await.unwrap();
        // Failing model → deterministic extractive fallback (keeps high-value
        // lines), so recall is checkable without asserting LLM output.
        let mock = Arc::new(MockLlmProvider::new());
        let bg = BackgroundMaintenance {
            db,
            session_id: session.id,
            llm: mock.clone() as Arc<dyn LlmProvider>,
            ctx_window: Some(32_768),
            in_flight: Arc::new(AtomicBool::new(false)),
        };

        let base = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut idx = 0i64;
        let mut all_ids: Vec<Uuid> = Vec::new();
        const TURNS: usize = 40;
        for t in 0..TURNS {
            let user_content = match t {
                0 => "DECISION: pricing is 42 USD per seat.".to_string(),
                1 => "Atlas migration scheduled 2026-09-15.".to_string(),
                5 => "URL: https://example.com/design-doc".to_string(),
                _ => format!("turn {t}: user discusses routine topic {t}."),
            };
            let msgs = vec![
                row_at(session.id, base, idx, "user", &user_content),
                row_at(
                    session.id,
                    base,
                    idx + 1,
                    "assistant",
                    &format!("turn {t}: assistant confirms and suggests next steps."),
                ),
                row_at(
                    session.id,
                    base,
                    idx + 2,
                    "tool",
                    &format!("turn {t}: tool result output {t} bytes ok."),
                ),
            ];
            idx += 3;
            for m in &msgs {
                all_ids.push(bg.db.add_message(m).await.unwrap().id);
            }

            // (d) current turn fully pending before maintenance runs.
            let (_, watermark) = bg.db.get_session_context(session.id).await.unwrap();
            let after = match &watermark {
                Some(wm) => bg
                    .db
                    .get_message_created_at(session.id, wm)
                    .await
                    .unwrap()
                    .unwrap_or_else(epoch),
                None => epoch(),
            };
            let pending = bg.db.get_messages_after(session.id, after).await.unwrap();
            assert_eq!(
                pending.len(),
                3,
                "current turn must be fully pending at turn {t}"
            );
            assert_eq!(pending[0].role, "user");
            assert_eq!(pending[1].role, "assistant");
            assert_eq!(pending[2].role, "tool");

            bg.maintain().await.expect("maintenance succeeds");
        }

        let (summary, watermark) = bg.db.get_session_context(session.id).await.unwrap();
        let summary = summary.expect("summary persisted");

        // (a) bounded: never grew past the summary cap across 40 requests.
        assert!(
            summary.chars().count() <= SUMMARY_CAP_TOKENS * 4 + 64,
            "summary must stay bounded, got {} chars",
            summary.chars().count()
        );

        // (b) early key facts survive in the durable prefix.
        assert!(
            summary.contains("42 USD per seat"),
            "early pricing fact recallable"
        );
        assert!(
            summary.contains("2026-09-15"),
            "early migration fact recallable"
        );
        assert!(summary.contains("example.com"), "early URL recallable");

        // (c) nothing evicted: all 120 messages still in the DB.
        let all = bg.db.get_messages(session.id, Some(1000)).await.unwrap();
        assert_eq!(
            all.len(),
            TURNS * 3,
            "messages are append-only, nothing evicted"
        );

        // Watermark advanced to the newest message (fully covered).
        let last_id = all_ids.last().expect("messages inserted").to_string();
        assert_eq!(watermark.as_deref(), Some(last_id.as_str()));
    }

    /// Spec deliverable 8 (acceptance), small-window case: a single request with
    /// a huge never-summarized backlog (~30K chars) at an 8K compaction window
    /// must be chunked into multiple LLM calls, produce a bounded summary, and
    /// leave the full backlog available before the pass runs.
    #[tokio::test]
    async fn one_shot_backlog_chunks_at_small_window() {
        let db = Database::connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory DB");
        let session = db.create_session("backlog").await.unwrap();

        let mut mock = MockLlmProvider::new();
        for _ in 0..16 {
            mock = mock.with_response(LlmResponse {
                content: Some("chunk key facts.".into()),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
            });
        }
        let mock = Arc::new(mock);

        // ~30K chars of never-summarized backlog.
        let base = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let filler = "user discusses the Atlas migration and the 42 Mbps limit. ";
        let mut total = 0usize;
        let mut i = 0i64;
        while total < 30_000 {
            let content = format!("backlog msg {i}: {filler}{filler}{filler}");
            total += content.len();
            db.add_message(&row_at(session.id, base, i, "user", &content))
                .await
                .unwrap();
            i += 1;
        }
        let backlog_count = i as usize;

        // Backlog fully available before the first maintenance pass.
        let pending = db.get_messages_after(session.id, epoch()).await.unwrap();
        assert_eq!(pending.len(), backlog_count, "backlog intact before pass");

        let bg = BackgroundMaintenance {
            db,
            session_id: session.id,
            llm: mock.clone() as Arc<dyn LlmProvider>,
            ctx_window: Some(8_000), // small window → forces chunking (D1)
            in_flight: Arc::new(AtomicBool::new(false)),
        };
        bg.maintain().await.expect("maintenance succeeds");

        assert!(
            mock.call_count() >= 2,
            "30K backlog at window 8K must chunk, got {} LLM calls",
            mock.call_count()
        );
        let (summary, watermark) = bg.db.get_session_context(session.id).await.unwrap();
        let summary = summary.expect("summary persisted");
        assert!(!summary.is_empty());
        assert!(
            summary.chars().count() <= SUMMARY_CAP_TOKENS * 4 + 64,
            "summary bounded, got {} chars",
            summary.chars().count()
        );
        // Watermark advanced past the whole backlog — next pass is a no-op.
        assert_eq!(
            watermark.as_deref(),
            Some(pending.last().unwrap().id.to_string().as_str())
        );
    }
}
