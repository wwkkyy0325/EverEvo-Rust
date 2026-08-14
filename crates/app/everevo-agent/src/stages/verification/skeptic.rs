//! EvidenceChecklist ContextStage — verifier-gated commit discipline.
//!
//! ECLoop-style (arXiv 2607.28815): pre-declare the evidence conditions an
//! answer must satisfy, then gate the commit step on each condition having a
//! source and a deterministic numeric check. Added at priority 3 (stable-sorts
//! right AFTER the modeling/verification ensemble — ProblemModeling and
//! VerifyCandidate at priority 3 — so the commit-gate prompt does not fire
//! before the agent has been told to model and verify; audit MEDIUM, 2026-08-13)
//! and covers the [C] "wrong committed answer" family in the GAIA L1 benchmark:
//!
//! - e1fc63a2: order-of-magnitude misread (claimed 17000, expected order 17)
//! - 3cef3a44: verbatim list dropped "fresh" / reordered
//! - e142056d: constraint misread ($12,000 vs 16000)
//!
//! The gate itself is prompt-level: the model runs the deterministic sandbox
//! verifier (`verify_candidate.py`) and only commits a `Final answer:` that
//! passes. The verifier loop is wall-clock capped so it can never consume the
//! question budget — when time is short the model commits its best candidate
//! rather than looping (never "no answer").

use super::gate::clamp_verify_fragment;
use super::gate::{classify, Difficulty};
use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects the pre-commit evidence checklist: enumerate the constraints the
/// answer must honor, verify each one deterministically, then commit.
///
/// Priority: 3 (stable-sorts right after ProblemModeling/VerifyCandidate so the
/// commit-gate prompt follows the modeling + verification instructions — see
/// pipeline.rs). Adaptive: only injected for `Hard` questions (see [`Difficulty`]).
pub struct EvidenceChecklistStage;

impl ContextStage for EvidenceChecklistStage {
    fn priority(&self) -> i32 {
        3
    }
    fn name(&self) -> &str {
        "evidence_checklist"
    }
    fn tool_visible(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Enumerate every number / unit / entity / operation the answer must honor; verify each deterministically (verify_candidate.py); escalate to cluster verify on disagreement."
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        // Simple questions: skip verification entirely (see Difficulty docs).
        if classify(&ctx.user_message) == Difficulty::Simple {
            return None;
        }
        let content = clamp_verify_fragment(
            &ctx.budget,
            "\
## Evidence Checklist (commit gate)

Act as an INDEPENDENT reviewer, not the author: your job is to REFUTE the
candidate answer with concrete evidence unless it survives every check below.

### 1. Enumerate before you start
In your FIRST response, write an explicit checklist of every NUMBER, UNIT, \
NAMED ENTITY, and OPERATION your final answer must honor. Examples:
\"order of magnitude ~10^2\", \"unit: hours\", \"entities: Y. Uehara\", \
\"operation: 16000 × 0.75\". Keep it visible so you can check each item \
against evidence later.

### 2. Verify before you commit (MANDATORY — the loop enforces this)
Before emitting `Final answer:`, every checklist item MUST have:
- a SOURCE (attached file, web result, or tool output that states it), and
- a DETERMINISTIC CHECK (a numeric computation, not a guess).
Run the sandbox verifier FIRST:
  `python verify_candidate.py verify --answer <your answer> \\
      --expected <expected value> [--unit <unit>] [--compute <expr>] \\
      [--recompute <expr2>] [--expect-list <verbatim items>] [--entity <name>]`
Use `--expected` with the value you derived and `--unit` with its dimension, \
so order-of-magnitude and unit errors are caught. For a NUMERIC aggregation \
answer, pass BOTH `--compute` (your formula) AND `--recompute` — a SECOND, \
independent method for the same value (different decomposition, unit path, or \
raw-data recount). The verifier rejects the candidate when the two methods \
disagree — a single formula that already embeds the mistake cannot pass alone. \
If it reports violations, repair the candidate and re-verify — at most 2 attempts.

### 3. Escalate to adversarial review if the deterministic check fails
If `verify_candidate.py` still reports violations after 2 repairs, run a \
second, INDEPENDENT check via the `cluster` tool:
  `cluster verify` with `claims` = [\"Final answer: <candidate>\"] and \
  `perspectives` = [\"numeric reviewer\", \"source-verbatim reviewer\"], \
  `asymmetric` = true (reviewer sees evidence, NOT your draft).
Commit only if it survives; the adversarial verdict is your tiebreaker.

### 3b. Dead / historical sources — pre-route to archives FIRST
When a question references a page that may be historical (\"as of 2020\", an \
old arXiv listing, a dead science site, a page that changed), do NOT retry the \
live page in a loop. Use `wayback_lookup`:
  `wayback_lookup` with `url`, `from`/`to` (e.g. \"20200101\"), and `action`:
  - `list` → archived snapshot URLs for a date range,
  - `raw` (+ optional `timestamp`) → the snapshot's RAW content (no toolbar).
If the live source is blocked by an anti-bot challenge (OpenReview, etc.), use \
a public mirror dataset (e.g. HuggingFace `creativityschapiro/openreview_raw`) \
instead of fighting the block. For live-API drift (e.g. World Bank), prefer a \
dated bulk download over the live API.

### 4. Cap the verify loop
Do not let re-verification consume the remaining time budget. If the verifier \
still disagrees after the repairs above, commit the BEST verified candidate \
anyway. Never respond \"no answer\" — a best-effort value beats an empty answer.

### 5. Order of magnitude first
If the question is numeric, sanity-check the magnitude before anything else: \
a 17000 where the quantity is ~17 is a wrong answer regardless of how the \
computation was \"derived\".

### 6. A verified item stays done
A checklist item that has a SOURCE and a DETERMINISTIC CHECK is DONE — do not \
re-fetch, re-search, or re-verify it. When every item on the checklist is done, \
COMMIT on the `Final answer:` line. Do not extend the checklist or invent new \
items after verification — an uncommitted verified answer scores 0.",
        );
        Some(ContextFragment {
            label: "Evidence Checklist".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}

/// Injects a mandatory pre-commit verification pass.
///
/// Priority: 3 (stable-sorts right after ProblemModeling, before
/// EvidenceChecklist — the commit gate — within the same-priority ensemble;
/// see pipeline.rs). Adaptive: only injected for `Hard` questions (see [`Difficulty`]).
pub struct VerifyCandidateStage;

impl ContextStage for VerifyCandidateStage {
    fn priority(&self) -> i32 {
        3
    }
    fn name(&self) -> &str {
        "verify_candidate"
    }
    fn tool_visible(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Re-derive the candidate from raw tool evidence; check precision / magnitude / units / counts / attribution; commit only a value that survives."
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        // Simple questions: skip verification entirely — research shows
        // mandatory verification harms accuracy on trivial requests and only
        // the difficulty gate keeps hard questions in the ensemble.
        if classify(&ctx.user_message) == Difficulty::Simple {
            return None;
        }
        let content = clamp_verify_fragment(
            &ctx.budget,
            "\
## Verify Candidate (HARD RULE — before the final answer)

You are an INDEPENDENT, skeptical REVIEWER of the candidate you are about to
commit — NOT its author. Do not trust your earlier derivation or first
instinct: re-derive the value from the raw tool evidence independently, and
commit only the value that survives ALL of the checks below.

### 0. ONE pass, then commit — verification is not a loop
Run the checks below ONCE against the raw evidence. If the candidate survives,
commit it on the `Final answer:` line IMMEDIATELY — do not restart research, do
not \"strengthen\" the answer with extra sources, do not re-run the same check a
second time \"to be sure\". If `verify_candidate.py` reports `circular`
(expected == answer), that check added NO independent evidence — re-derive via a
DIFFERENT path (recompute from raw data, or `cluster verify`), never dismiss
the warning as expected. A verified candidate that is never committed scores 0.

### 1. Numeric answers — recompute, do not trust the first result
If the answer is a number, RE-EXECUTE the computation (shell/python) with the
raw source data, or recompute from the extracted numbers, and confirm the
result matches. Specifically catch these recurring failure patterns:
- **Precision / rounding:** output the EXACT value, not a rounded or truncated
  form. `0.0424` is not `0.0429`; `1.456` is not `1.46`. If the source gives
  more digits, keep them.
- **Magnitude / scale:** check the answer is the right order of magnitude.
  `101.376` is not `768`; `26.4` is not `2.0`; `0.2` is not `0.02`. Re-derive
  from the raw numbers.
- **Units:** the answer must be in the units the QUESTION asks for. If you
  converted, re-check the conversion factor.
- **Under-count / over-count:** if the answer is a count, enumerate the items
  programmatically from the source and recount. The count must match the
  source data, not a memory of it. `3` is not `6`; `55` is not `225`.

### 2. Non-numeric answers — check against the source
If the answer is a string/name/title, it MUST appear verbatim in a tool result
you actually retrieved. Re-read the source line that contains it. If the
answer is a list, verify every item (and no more) is present in the source.

### 3. Attribution answers — per-record mapping
If the answer identifies WHICH record/country/item has a property, re-read each
candidate record's OWN fields and confirm the attribution maps record→property
correctly. Never answer with a country/entity whose record you did not parse.

### 4. Time / date answers
If the answer is a time or date, recompute the conversion (timezone, format,
calendar) from the raw source values. `6:41 PM` is not `6:12 PM`; a year must
not be off by one.

### 5. If the check fails — do NOT commit
If the verification pass disagrees with your candidate, DISCARD the candidate,
re-derive from the source, and re-verify. Only when the verified value matches
the recomputation AND the source do you commit it on the `Final answer:` line.
An uncertain-but-verified value beats a confident-but-unverified one — but a
verified WRONG value is still wrong, so verify against the SOURCE, not your
own reasoning.",
        );
        Some(ContextFragment {
            label: "Verify Candidate".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}
