//! Context maintenance — durable rolling summary with budget-aware chunking.
//!
//! Implemented against `docs/agent-context-management-spec.md`:
//! rule 1 (never re-summarize summaries — old summary is kept verbatim as a
//! prefix), rule 5 (soft threshold), rule 6 (main loop never blocks), and the
//! cheap-model context-budget requirement (D1).

pub mod background;
pub mod rolling_summary;

pub use background::BackgroundMaintenance;
pub use rolling_summary::{maintain_rolling_summary, RollingSummaryResult, SUMMARY_CAP_TOKENS};
