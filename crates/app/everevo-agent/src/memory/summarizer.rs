//! Summarizer Agent — session-end structured handoff + paradigm extraction.
//!
//! ## Role
//!
//! The Summarizer runs once at session end (or on session suspend). It
//! compresses the full session into:
//!
//! 1. A **structured handoff summary** saved as a Reference fact, injectable
//!    into the next session's context
//! 2. **Extracted paradigms** saved as Paradigm facts for future reuse
//! 3. **Key decisions + open issues** for the user to review
//!
//! ## Follows the same fire-and-forget pattern as `reflect_on_turn()`
//!
//! The summarizer is spawned as a background task on session close.
//! Failures are logged, never surfaced.
//!
//! ## Contrastive design
//!
//! The summary explicitly contrasts WHAT WORKED vs WHAT DIDN'T, enabling
//! the next session to skip dead ends and reuse successful approaches.

use everevo_core::llm::{LlmMessage, LlmProvider};
use everevo_core::memory::{FactType, MemoryFact, ParadigmLevel, ParadigmMeta, ProjectionMetadata};
use uuid::Uuid;

use crate::llm::HttpClient;
use crate::memory::extractor::parse_extracted_facts;
use crate::memory::facts::FactManager;
use crate::memory::paradigm::TrajectoryBuffer;

// ── Session Summary ─────────────────────────────────────────────────────────

/// Structured output of the Summarizer agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSummary {
    /// What was achieved.
    #[serde(default)]
    pub goals_achieved: Vec<String>,
    /// What was attempted but failed.
    #[serde(default)]
    pub goals_failed: Vec<FailedGoal>,
    /// Key decisions and their rationale.
    #[serde(default)]
    pub key_decisions: Vec<Decision>,
    /// Issues that remain unresolved.
    #[serde(default)]
    pub open_issues: Vec<String>,
    /// Context to inject into the NEXT session (Claude Code handoff style).
    pub handoff_context: String,
    /// Number of paradigms extracted from this session.
    #[serde(default)]
    pub paradigms_extracted: u32,
    /// Session ID this summary belongs to.
    pub session_id: Uuid,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedGoal {
    pub goal: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub what: String,
    pub why: String,
}

impl SessionSummary {
    /// Format the handoff for injection into the next session's context.
    pub fn format_handoff(&self) -> String {
        let mut text = String::from("## Previous Session Summary\n\n");

        if !self.goals_achieved.is_empty() {
            text.push_str("### Achieved\n");
            for g in &self.goals_achieved {
                text.push_str(&format!("- ✅ {g}\n"));
            }
            text.push('\n');
        }

        if !self.goals_failed.is_empty() {
            text.push_str("### Failed / Incomplete\n");
            for f in &self.goals_failed {
                let reason = f
                    .reason
                    .as_ref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                text.push_str(&format!("- ❌ {}{reason}\n", f.goal));
            }
            text.push('\n');
        }

        if !self.key_decisions.is_empty() {
            text.push_str("### Key Decisions\n");
            for d in &self.key_decisions {
                text.push_str(&format!("- **{}**: {}\n", d.what, d.why));
            }
            text.push('\n');
        }

        if !self.open_issues.is_empty() {
            text.push_str("### Open Issues\n");
            for i in &self.open_issues {
                text.push_str(&format!("- ⚠️ {i}\n"));
            }
            text.push('\n');
        }

        if !self.handoff_context.is_empty() {
            text.push_str("### Context for This Session\n");
            text.push_str(&self.handoff_context);
        }

        text
    }
}

// ── Summarize Session ──────────────────────────────────────────────────────

/// Run the Summarizer on a completed session.
///
/// Fire-and-forget (same pattern as `reflect_on_turn`). Called once at
/// session close.
///
/// Saves the summary as a `FactType::Reference` fact and any extracted
/// paradigms as `FactType::Paradigm` facts.
pub async fn summarize_session(
    llm: &HttpClient,
    fact_manager: &FactManager,
    trajectory: &TrajectoryBuffer,
    session_id: Uuid,
    user_goals: &str,
    full_history_summary: &str,
) -> SessionSummary {
    let snap = trajectory.snapshot();

    // Build summary prompt
    let prompt = build_summary_prompt(&snap, user_goals, full_history_summary);
    let messages = vec![LlmMessage::user(&prompt)];

    let response = match llm.chat(&messages, &[]).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Summarizer LLM call failed");
            return SessionSummary {
                goals_achieved: Vec::new(),
                goals_failed: Vec::new(),
                key_decisions: Vec::new(),
                open_issues: Vec::new(),
                handoff_context: String::new(),
                paradigms_extracted: 0,
                session_id,
            };
        }
    };

    let text = response.content.unwrap_or_default();
    let summary = parse_summary_response(&text, session_id);

    // Save summary as a Reference fact
    let fact_content = summary.format_handoff();
    if !fact_content.is_empty() {
        let fact = MemoryFact {
            name: format!("session-summary-{}", session_id),
            description: format!(
                "Session summary: {} achieved, {} failed, {} open",
                summary.goals_achieved.len(),
                summary.goals_failed.len(),
                summary.open_issues.len()
            ),
            content: fact_content,
            fact_type: FactType::Reference,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("summarizer-agent", "llm", vec![], 0.7),
            links: vec![],
            // Session handoff summary — deliberately cross-session (global tier).
            session: Some("global".into()),
        };
        let _ = fact_manager.save_async(fact).await;
    }

    // Extract paradigms from session (if enough trajectory data)
    let paradigms_extracted = if snap.len() >= 3 {
        extract_session_paradigms(llm, fact_manager, &snap, session_id).await
    } else {
        0
    };

    SessionSummary {
        paradigms_extracted,
        ..summary
    }
}

/// Extract paradigms from the session trajectory.
async fn extract_session_paradigms(
    llm: &HttpClient,
    fact_manager: &FactManager,
    trajectory: &[crate::memory::paradigm::TurnDigest],
    session_id: Uuid,
) -> u32 {
    let prompt = build_session_paradigm_prompt(trajectory);
    let messages = vec![LlmMessage::user(&prompt)];

    let response = match llm.chat(&messages, &[]).await {
        Ok(r) => r,
        Err(_) => return 0,
    };

    let text = response.content.unwrap_or_default();
    let facts = parse_extracted_facts(&text);
    let mut saved = 0u32;

    for (name, description, content) in &facts {
        let meta = ParadigmMeta {
            problem_class: name.clone(),
            preconditions: Vec::new(),
            approach: description.clone(),
            parameters: Vec::new(),
            success_signals: Vec::new(),
            failure_modes: Vec::new(),
            divergence_point: None,
            anti_pattern: None,
            extraction_level: ParadigmLevel::Meso,
        };
        let meta_json = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into());
        let full_content = format!(
            "# Paradigm: {name}\n\n**Approach**: {description}\n\n{content}\n\n---\n```json\n{meta_json}\n```"
        );

        let fact = MemoryFact {
            name: format!("paradigm-session-{session_id}-{name}"),
            description: description.clone(),
            content: full_content,
            fact_type: FactType::Paradigm,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            projection: ProjectionMetadata::new("summarizer-agent", "llm", vec![], 0.65),
            links: vec![],
            // Reusable action paradigm — deliberately cross-session (global tier).
            session: Some("global".into()),
        };
        match fact_manager.save_async(fact).await {
            Ok(()) => saved += 1,
            Err(e) => tracing::debug!(error = %e, "Summarizer paradigm save dedup'd"),
        }
    }

    if saved > 0 {
        tracing::info!(saved, %session_id, "Summarizer: extracted session paradigms");
    }
    saved
}

// ── Prompt Builders ─────────────────────────────────────────────────────────

fn build_summary_prompt(
    trajectory: &[crate::memory::paradigm::TurnDigest],
    user_goals: &str,
    history_summary: &str,
) -> String {
    let mut turns = String::new();
    for (i, t) in trajectory.iter().enumerate() {
        let status = if t.success { "✓" } else { "✗" };
        let err = t
            .error_type
            .as_ref()
            .map(|e| format!(" [{}]", e))
            .unwrap_or_default();
        turns.push_str(&format!("  T{}: {} {}{err}\n", i + 1, t.tool_name, status));
    }

    format!(
        "You are the Summarizer Agent. Analyze this completed AI agent session and produce \
         a structured summary.\n\n\
         ## Session Data\n\
         User Goals: {user_goals}\n\
         Conversation Summary: {history_summary}\n\n\
         ## Execution Trajectory\n\
         {turns}\n\n\
         ## Instructions\n\
         Return a JSON object with exactly these fields:\n\
         {{\n\
           \"goals_achieved\": [\"list of what was accomplished\"],\n\
           \"goals_failed\": [{{\"goal\": \"...\", \"reason\": \"why\"}}],\n\
           \"key_decisions\": [{{\"what\": \"...\", \"why\": \"...\"}}],\n\
           \"open_issues\": [\"list of remaining problems\"],\n\
           \"handoff_context\": \"1-3 sentences for the next session: what approach works, \
             what to avoid, current state of the workspace\"\n\
         }}\n\n\
         Be HONEST about failures. The next session needs to know what NOT to retry.\n\n\
         JSON:"
    )
}

fn build_session_paradigm_prompt(trajectory: &[crate::memory::paradigm::TurnDigest]) -> String {
    let mut turns = String::new();
    for (i, t) in trajectory.iter().enumerate() {
        let status = if t.success { "✓" } else { "✗" };
        turns.push_str(&format!(
            "  T{}: {} {} | {}\n",
            i + 1,
            t.tool_name,
            status,
            t.user_intent
        ));
    }

    format!(
        "Extract reusable ACTION PARADIGMS from this session's execution trajectory.\n\n\
         ## Trajectory\n{turns}\n\n\
         ## Instructions\n\
         Extract 1-3 paradigms as a JSON array. Each entry:\n\
         {{\"name\": \"kebab-case-slug\", \"description\": \"one-line strategy\", \
         \"content\": \"detailed paradigm: when to apply, what approach to use, what pitfalls to avoid\"}}\n\n\
         If the session is too trivial (just greetings, simple lookups), return: [NO_PARADIGM]\n\n\
         JSON:"
    )
}

// ── Response Parser ─────────────────────────────────────────────────────────

fn parse_summary_response(text: &str, session_id: Uuid) -> SessionSummary {
    // Try to find JSON object in the response
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find { ... } boundaries
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    if let (Some(s), Some(e)) = (start, end) {
        let json_str = &cleaned[s..=e];
        if let Ok(summary) = serde_json::from_str::<SessionSummary>(json_str) {
            return SessionSummary {
                session_id,
                ..summary
            };
        }
    }

    // Fallback: build minimal summary from plain text
    SessionSummary {
        goals_achieved: Vec::new(),
        goals_failed: Vec::new(),
        key_decisions: Vec::new(),
        open_issues: Vec::new(),
        handoff_context: text.lines().take(3).collect::<Vec<_>>().join(" "),
        paradigms_extracted: 0,
        session_id,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_summary_format_handoff() {
        let summary = SessionSummary {
            goals_achieved: vec!["Deployed app".into()],
            goals_failed: vec![FailedGoal {
                goal: "Setup monitoring".into(),
                reason: Some("Permission denied".into()),
            }],
            key_decisions: vec![Decision {
                what: "Used Docker Compose".into(),
                why: "Faster than k8s for dev".into(),
            }],
            open_issues: vec!["SSL cert expiring soon".into()],
            handoff_context: "The app is running on port 3000.".into(),
            paradigms_extracted: 2,
            session_id: Uuid::nil(),
        };

        let handoff = summary.format_handoff();
        assert!(handoff.contains("✅ Deployed app"));
        assert!(handoff.contains("❌ Setup monitoring"));
        assert!(handoff.contains("Docker Compose"));
        assert!(handoff.contains("⚠️ SSL cert"));
        assert!(handoff.contains("port 3000"));
    }

    #[test]
    fn test_session_summary_empty() {
        let summary = SessionSummary {
            goals_achieved: vec![],
            goals_failed: vec![],
            key_decisions: vec![],
            open_issues: vec![],
            handoff_context: String::new(),
            paradigms_extracted: 0,
            session_id: Uuid::nil(),
        };
        let handoff = summary.format_handoff();
        assert!(handoff.contains("Previous Session Summary"));
    }

    #[test]
    fn test_parse_summary_json() {
        let json = r#"{"goals_achieved":["Built app"],"goals_failed":[],"key_decisions":[{"what":"Used npm","why":"Available"}],"open_issues":[],"handoff_context":"App is in dist/ folder. Use npm start to run.","paradigms_extracted":1,"session_id":"00000000-0000-0000-0000-000000000000"}"#;
        let summary = parse_summary_response(json, Uuid::nil());
        assert_eq!(summary.goals_achieved.len(), 1);
        assert_eq!(summary.goals_achieved[0], "Built app");
        assert_eq!(summary.key_decisions[0].what, "Used npm");
        assert!(summary.handoff_context.contains("dist/"));
    }

    #[test]
    fn test_parse_summary_fallback() {
        let text = "We built the app successfully. It's running on port 3000. Use npm start.";
        let summary = parse_summary_response(text, Uuid::nil());
        assert!(summary.handoff_context.contains("port 3000"));
    }

    #[test]
    fn test_paradigm_prompt_uses_trajectory() {
        let digests = vec![
            crate::memory::paradigm::TurnDigest::new("shell", true, None, "build", "ok"),
            crate::memory::paradigm::TurnDigest::new("shell", false, Some("err"), "deploy", "fail"),
        ];
        let prompt = build_session_paradigm_prompt(&digests);
        assert!(prompt.contains("build"));
        assert!(prompt.contains("deploy"));
    }
}
