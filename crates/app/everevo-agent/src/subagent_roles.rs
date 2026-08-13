//! Agent-role vocabulary + soft tier layering for the prompt pipeline.
//!
//! Builds on the prior three-tier design (archived `agent-orchestration.md`:
//! Supervisor / SubAgent / Skill) rather than cutting the existing system into
//! a hard hierarchy. Two soft structures live here:
//!
//! 1. **`AgentRole`** — a SHARED ROLE VOCABULARY. Existing role systems
//!    (`stype_guidance` in delegate/spawn.rs, `TeamRole` in team.rs) keep their
//!    own prompts and behavior; this enum gives them one set of names and an
//!    optional canonical prompt provider that NEW roles / extensions can opt
//!    into. Extensible: add a variant, no structural change.
//! 2. **`AgentTier`** — SOFT LAYERING METADATA. Every role maps to a tier
//!    (Supervisor / SubAgent / Verifier). It is annotation, not structure:
//!    nothing is cut or moved; the tier is where the existing components land
//!    and future layers can slot in.
//!
//! | Tier | Roles | Existing component |
//! |---|---|---|
//! | `Supervisor` | `Orchestrator` | the main agent loop (decomposes/delegates) |
//! | `SubAgent` | `Researcher`, `Coder`, `FileOps`, `General` | task/team/cluster sub-agents |
//! | `Verifier` | `Verifier` | `cluster verify` / `verify_candidate.py` reviewers |
//! | (skills) | — | `SkillStage` (expertise layer, orthogonal) |

use serde::{Deserialize, Serialize};

/// The soft three-tier layering of the agent system (Supervisor / SubAgent /
/// Verifier), used as metadata for the prompt pipeline and tool scoping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTier {
    /// The main agent: owns the loop, decomposes, delegates, adjudicates.
    Supervisor,
    /// Bounded task executors spawned by the supervisor.
    SubAgent,
    /// Independent adversarial reviewers (multi-party verification).
    Verifier,
}

impl AgentTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentTier::Supervisor => "supervisor",
            AgentTier::SubAgent => "sub_agent",
            AgentTier::Verifier => "verifier",
        }
    }
}

/// A sub-agent's role — determines its system prompt and (via the tool
/// registries) its tool surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Supervisor tier — the main agent / orchestrator.
    Orchestrator,
    /// SubAgent tier — codebase / web researcher.
    Researcher,
    /// SubAgent tier — implementation engineer.
    Coder,
    /// SubAgent tier — precise file operator.
    FileOps,
    /// Verifier tier — adversarial reviewer.
    Verifier,
    /// SubAgent tier — unspecialized fallback.
    General,
}

impl AgentRole {
    /// The soft tier this role belongs to (metadata, not structure).
    pub fn tier(&self) -> AgentTier {
        match self {
            AgentRole::Orchestrator => AgentTier::Supervisor,
            AgentRole::Verifier => AgentTier::Verifier,
            _ => AgentTier::SubAgent,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRole::Orchestrator => "orchestrator",
            AgentRole::Researcher => "researcher",
            AgentRole::Coder => "coder",
            AgentRole::FileOps => "file_ops",
            AgentRole::Verifier => "verifier",
            AgentRole::General => "general",
        }
    }

    /// Parse a role name, accepting the legacy aliases from the old systems.
    /// `code-explorer` maps to Researcher (the historical default sub-agent
    /// type) — preserving prior behavior; `tester` keeps its own semantics in
    /// the team tool and is NOT folded into Verifier here.
    pub fn parse(s: &str) -> Self {
        match s {
            "orchestrator" | "main" => AgentRole::Orchestrator,
            "research" | "researcher" | "code-explorer" => AgentRole::Researcher,
            "coder" => AgentRole::Coder,
            "file_ops" | "file" => AgentRole::FileOps,
            "verifier" | "reviewer" => AgentRole::Verifier,
            _ => AgentRole::General,
        }
    }

    /// Optional canonical prompt provider for this role. Existing systems
    /// (`stype_guidance`, `TeamRole`) may keep their own prompts; this is the
    /// shared vocabulary + a provider new roles / extensions opt into.
    pub fn system_prompt(&self) -> &'static str {
        match self {
            AgentRole::Orchestrator => {
                "\
## Role: Orchestrator

You are the orchestrator of this task. Your job is to:
1. Decompose the user's goal into sub-tasks.
2. Delegate sub-tasks to focused sub-agents (research / code / file) where \
specialization helps.
3. Verify results before committing — for a hard/complex task, run a \
verification step (deterministic check or an adversarial reviewer) rather \
than trusting your first answer.
4. Synthesize the delegated results into the final answer.

Rules:
- Do not do everything yourself when a focused sub-agent is clearly better \
suited; but do not over-delegate trivial steps (ChromaFlow: over-orchestration \
harms accuracy).
- Every claim in your final answer must trace to a retrieved source or a \
verified computation."
            }
            AgentRole::Researcher => {
                "\
## Role: Researcher

You are a thorough researcher. Focus on:
- Exploring all relevant files and patterns
- Finding connections across modules
- Documenting your findings with file paths and line numbers
- Providing a structured, comprehensive report

Leave no stone unturned. Every claim must have a file:line reference."
            }
            AgentRole::Coder => {
                "\
## Role: Coder

You are a precise implementation engineer. Focus on:
- Reading the relevant code before changing anything
- Making the minimal changes needed
- Matching existing code style and conventions
- Verifying with `cargo check` / tests after changes
- Reporting exactly what changed and why

Never weaken tests. If a test fails, the code is wrong — not the test."
            }
            AgentRole::FileOps => {
                "\
## Role: File Operations

You are a precise file operator. Focus on:
- Making the requested file changes exactly as specified
- Verifying each change with tests or checks
- Leaving no unintended side effects
- Reporting what was changed and why."
            }
            AgentRole::Verifier => {
                "\
## Role: Verifier (adversarial reviewer)

You are a critical, adversarial reviewer — NOT the author of the candidate.
Your job is to REFUTE the candidate with concrete evidence unless it survives
every check. Focus on:
- Correctness bugs and edge cases
- Numeric/unit/magnitude errors (recompute independently)
- Verbatim fidelity against the source (names, lists, counts)
- Security vulnerabilities, performance issues, test-coverage gaps

Be thorough and adversarial — find every issue. Default to REFUTED if
uncertain; provide specific evidence for your verdict."
            }
            AgentRole::General => {
                "\
## Role: General Assistant

Complete the assigned task thoroughly and return a structured result with \
evidence (file paths, line numbers, test results)."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_legacy_aliases() {
        assert_eq!(AgentRole::parse("reviewer"), AgentRole::Verifier);
        // `code-explorer` was the historical DEFAULT sub-agent type and mapped
        // to the researcher prompt — preserved as Researcher.
        assert_eq!(AgentRole::parse("code-explorer"), AgentRole::Researcher);
        assert_eq!(AgentRole::parse("file"), AgentRole::FileOps);
        assert_eq!(AgentRole::parse("research"), AgentRole::Researcher);
        assert_eq!(AgentRole::parse("researcher"), AgentRole::Researcher);
        assert_eq!(AgentRole::parse("orchestrator"), AgentRole::Orchestrator);
        assert_eq!(AgentRole::parse("unknown-role"), AgentRole::General);
        // `tester` keeps its own semantics in the team tool — not folded here.
        assert_eq!(AgentRole::parse("tester"), AgentRole::General);
    }

    #[test]
    fn test_system_prompt_is_non_empty() {
        for role in [
            AgentRole::Orchestrator,
            AgentRole::Researcher,
            AgentRole::Coder,
            AgentRole::FileOps,
            AgentRole::Verifier,
            AgentRole::General,
        ] {
            assert!(!role.system_prompt().is_empty());
            // Every prompt names its role.
            assert!(role.system_prompt().contains("Role:"));
        }
    }

    #[test]
    fn test_as_str_roundtrip() {
        for role in [
            AgentRole::Orchestrator,
            AgentRole::Researcher,
            AgentRole::Coder,
            AgentRole::FileOps,
            AgentRole::Verifier,
            AgentRole::General,
        ] {
            assert_eq!(AgentRole::parse(role.as_str()), role);
        }
    }

    #[test]
    fn test_tier_mapping() {
        // Supervisor tier = the main agent.
        assert_eq!(AgentRole::Orchestrator.tier(), AgentTier::Supervisor);
        // SubAgent tier = executors + fallback.
        for r in [
            AgentRole::Researcher,
            AgentRole::Coder,
            AgentRole::FileOps,
            AgentRole::General,
        ] {
            assert_eq!(r.tier(), AgentTier::SubAgent);
        }
        // Verifier tier = adversarial reviewers.
        assert_eq!(AgentRole::Verifier.tier(), AgentTier::Verifier);
    }
}
