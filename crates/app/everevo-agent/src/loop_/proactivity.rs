//! Fixation-loop detection state machine (L1-L4 escalation) plus the
//! signature-hashing helpers used to distinguish repeated attempts.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Tracks fixation patterns across ReAct turns and escalates from gentle hints
/// to forced divergence. Design references:
/// - PUA Skill (tanweai): L1-L4 escalating pressure with mandatory actions
/// - Replit Decision-Time Guidance: ephemeral injections at decision points
/// - HASP (arXiv 2605.17734): executable guardrails with activation predicates
#[derive(Debug, Clone)]
pub struct ProactivityState {
    /// Current escalation level.
    pub level: EscalationLevel,
    /// Hash of last error signature (tool_name + error_substr) for dedup.
    last_error_sig: Option<u64>,
    /// Consecutive turns with the same error signature.
    same_error_count: u32,
    /// Whether WebSearch was used since the last escalation trigger.
    pub has_researched: bool,
    /// Count of distinct tool+arg combinations tried (proxy for "approaches").
    distinct_approaches: u32,
}

/// Escalation levels for fixation-loop intervention.
///
/// Level 0 (Normal) carries no overhead — no messages injected, no state tracked
/// beyond the default struct. Cost scales with escalation: nothing at L0, a single
/// line at L1, a paragraph at L2, a checklist at L3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscalationLevel {
    /// Normal operation — no fixation detected.
    Normal = 0,
    /// First repeat: same tool + same error once. Gentle nudge.
    Hint = 1,
    /// Second repeat: web research required before retrying.
    ResearchRequired = 2,
    /// Third+ repeat: must enumerate fundamentally different approaches.
    ForcedDivergence = 3,
}

impl ProactivityState {
    pub fn new() -> Self {
        Self {
            level: EscalationLevel::Normal,
            last_error_sig: None,
            same_error_count: 0,
            has_researched: false,
            distinct_approaches: 0,
        }
    }

    /// Update state after a tool execution. Call once per tool result.
    ///
    /// `tool_name` — name of the executed tool.
    /// `is_error` — whether the result is an error.
    /// `args_hash` — a stable hash of the tool arguments, for distinguishing
    ///   "same approach" from "different approach."
    /// `prev_tool_sig` — the previous turn's (tool_name, args_hash), if any.
    pub fn update(
        &mut self,
        tool_name: &str,
        is_error: bool,
        args_hash: u64,
        prev_tool_sig: Option<(&str, u64)>,
    ) {
        if !is_error {
            // Non-error result → check if approach changed.
            if let Some((prev_name, prev_hash)) = prev_tool_sig {
                if prev_name != tool_name || prev_hash != args_hash {
                    // New approach + success → reset completely.
                    self.reset();
                    self.distinct_approaches += 1;
                    return;
                }
            }
            // Same approach succeeded — no escalation needed, but don't reset
            // (the success might be fragile; keep light tracking).
            return;
        }

        // Error path: compute signature and compare.
        let sig = hash_str(tool_name);

        if self.last_error_sig == Some(sig) {
            self.same_error_count += 1;
        } else {
            self.same_error_count = 1;
            self.last_error_sig = Some(sig);
            // New error type → check if approach changed.
            if let Some((prev_name, prev_hash)) = prev_tool_sig {
                if prev_name != tool_name || prev_hash != args_hash {
                    self.distinct_approaches += 1;
                }
            }
        }

        // Escalate based on same_error_count.
        self.level = match self.same_error_count {
            0..=1 => EscalationLevel::Normal,
            2 => EscalationLevel::Hint,
            3 => EscalationLevel::ResearchRequired,
            _ => EscalationLevel::ForcedDivergence,
        };
    }

    /// Build the intervention message to inject into the conversation, if any.
    /// Returns None at L0, a one-liner at L1, a paragraph at L2, a checklist at L3.
    pub fn intervention_message(&self) -> Option<String> {
        match self.level {
            EscalationLevel::Normal => None,
            EscalationLevel::Hint => Some(
                "\
[SYSTEM NOTE] Your last attempt with the same approach failed. \
Do NOT retry with minor parameter changes — it will fail again. \
Consider: is there a DIFFERENT tool or strategy? \
(SSH failing? Use HTTPS + token. API call failing? Use a different library. \
Command not found? Check what's installed with `which`.)"
                    .into(),
            ),
            EscalationLevel::ResearchRequired => Some(
                "\
## [REQUIRED] Research Before Retrying\n\n\
You have attempted the same approach twice and both failed. \
Before your next attempt you MUST:\n\
1. Call web_search for at least 2 relevant queries (include the exact error)\n\
2. Read the results and identify root causes\n\
3. Choose a FUNDAMENTALLY different approach — not just parameter tweaks\n\
   (e.g., SSH→HTTPS, one library→another, direct call→CLI tool)\n\
4. Explain your NEW approach before executing it\n\n\
If this is a connectivity issue (SSH, network), check: do you have a token \
configured? Use HTTPS with the token — it's already in the sandbox env."
                    .into(),
            ),
            EscalationLevel::ForcedDivergence => Some(
                "\
## [REQUIRED] Forced Divergence — Same Approach Failed 3+ Times\n\n\
You are stuck in a fixation loop. STOP retrying immediately.\n\n\
Complete ALL of these before ANY further action:\n\
- [ ] Re-read the LAST error message word-for-word — what EXACTLY failed?\n\
- [ ] web_search: the exact error message (copy-paste it)\n\
- [ ] web_search: alternative approaches to {your task}\n\
- [ ] List 3 DISTINCT hypotheses for why this fails\n\
- [ ] Choose the best alternative and explain WHY it will work\n\n\
**Common root causes for persistent failures**:\n\
- SSH to GitHub fails → use HTTPS + GH_TOKEN (it's in sandbox env)\n\
- Package install fails → check if the runtime is available (`which python`)\n\
- Build fails → read the ACTUAL error line, not the summary\n\
- Connection refused → the service may not be running; check with curl\n\
- Permission denied → you're in a sandbox; explain what you need\n\n\
Your NEXT action MUST be fundamentally different. If you truly cannot find \
an alternative, say: \"I've tried X, Y, Z. Here's what failed and what I need.\" \
Honesty about failure is better than an infinite retry loop."
                    .into(),
            ),
        }
    }

    /// Record that the agent used a research tool (web_search, web_fetch).
    pub fn mark_researched(&mut self) {
        self.has_researched = true;
    }

    fn reset(&mut self) {
        self.level = EscalationLevel::Normal;
        self.last_error_sig = None;
        self.same_error_count = 0;
        self.has_researched = false;
    }
}

impl Default for ProactivityState {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

pub(crate) fn hash_args(args: &serde_json::Value) -> u64 {
    let mut h = DefaultHasher::new();
    args.to_string().hash(&mut h);
    h.finish()
}
