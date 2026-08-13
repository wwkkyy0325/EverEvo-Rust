//! Deterministic question-difficulty classifier for adaptive verification.
//!
//! Free (no LLM call): a pure heuristic over the question text that decides
//! whether a request needs the multi-party verification ensemble. Research
//! (VerifiAgent; Leni arXiv 2607.17044) shows verification is net-positive
//! only on hard questions and can *hurt* on simple ones — so simple questions
//! skip verification entirely and pay zero extra tokens.
//!
//! The classifier is deliberately conservative (errs toward `Hard`): the cost
//! asymmetry is one-sided — classifying a simple question as hard only spends
//! a few extra tokens, while classifying a hard question as simple risks a
//! wrong answer. Only genuinely trivial requests fall through to `Simple`.

/// Per-request verification need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    /// Trivial request — skip verification stages and commit directly.
    Simple,
    /// Complex request — inject the full multi-party verification ensemble.
    Hard,
}

/// Long-question threshold (multi-hop indicator).
const LONG_TEXT_CHARS: usize = 300;

/// Count/list/selection verbs that demand programmatic enumeration.
const COUNT_WORDS: &[&str] = &[
    "how many",
    "count",
    "list",
    "enumerate",
    "most",
    "fewest",
    "each",
    "number of",
    "which ",
    "total",
];

/// Ambiguity / precision-sensitive markers that demand care.
const AMBIGUITY_WORDS: &[&str] = &[
    "at least",
    "at most",
    "excluding",
    "among",
    "between",
    "approximately",
    "estimate",
    "roughly",
    "compare",
    "difference",
    "convert",
    "minus",
    "plus",
    "times",
];

/// How hard the question looks. Higher = stronger evidence of difficulty.
///
/// Rules (any strong signal is enough to make the question `Hard`):
/// - attachment marker `[Attached file:` → file must be parsed
/// - a digit → numeric recompute / unit / magnitude verification needed
/// - a count/list verb → programmatic enumeration needed
/// - an ambiguity marker → constraint-reading care needed
/// - question length above threshold → multi-hop
/// - substantive CJK content (8+ han chars, or a CJK question/comparison marker)
///   → the English keyword tables don't fire on han text (audit MEDIUM, 2026-08-13)
///
/// CJK question/comparison markers that signal a real question (not a greeting).
const CJK_MARKERS: &[&str] = &[
    "哪个",
    "多少",
    "什么",
    "比较",
    "分别",
    "如何",
    "为什么",
    "是否",
    "？",
    "几",
];

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0x3400..=0x4DBF // Extension A
        | 0xF900..=0xFAFF // Compatibility
        | 0x3040..=0x30FF // Hiragana + Katakana
        | 0xAC00..=0xD7AF // Hangul
    )
}

pub fn hard_score(text: &str) -> u32 {
    let lower = text.to_lowercase();
    let mut score = 0u32;

    if text.contains("[Attached file:") || text.contains("[attached file:") {
        score += 10;
    }
    if lower.chars().any(|c| c.is_ascii_digit()) {
        score += 10;
    }
    if COUNT_WORDS.iter().any(|w| lower.contains(w)) {
        score += 10;
    }
    if AMBIGUITY_WORDS.iter().any(|w| lower.contains(w)) {
        score += 5;
    }
    if text.chars().count() > LONG_TEXT_CHARS {
        score += 5;
    }
    // CJK content: the English keyword tables can't fire on han text, but a
    // substantive CJK question (8+ han chars, or a CJK question/comparison
    // marker) needs research/verification. Trivial greetings ("你好") stay
    // below the bar and remain Simple (2026-08-13 fix, audit MEDIUM).
    let cjk_count = text.chars().filter(|c| is_cjk(*c)).count();
    if cjk_count >= 8 || CJK_MARKERS.iter().any(|m| text.contains(m)) {
        score += 10;
    }

    score
}

/// Classify a request. `Simple` only for genuinely trivial text.
pub fn classify(user_message: &str) -> Difficulty {
    if hard_score(user_message) > 0 {
        Difficulty::Hard
    } else {
        Difficulty::Simple
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_greeting() {
        assert_eq!(classify("你好"), Difficulty::Simple);
        assert_eq!(classify("Hi, how are you?"), Difficulty::Simple);
        assert_eq!(classify("thanks"), Difficulty::Simple);
    }

    #[test]
    fn test_numeric_is_hard() {
        assert_eq!(classify("What is 2 + 2?"), Difficulty::Hard);
        assert_eq!(
            classify("In what year was she born in 1924?"),
            Difficulty::Hard
        );
    }

    #[test]
    fn test_count_verb_is_hard() {
        assert_eq!(classify("How many species are listed?"), Difficulty::Hard);
        assert_eq!(classify("List the ingredients."), Difficulty::Hard);
        assert_eq!(
            classify("Which country has the most volcanoes?"),
            Difficulty::Hard
        );
    }

    #[test]
    fn test_ambiguity_is_hard() {
        assert_eq!(
            classify("At least how many coins per box?"),
            Difficulty::Hard
        );
        assert_eq!(
            classify("Excluding the outliers, what is the mean?"),
            Difficulty::Hard
        );
        assert_eq!(
            classify("Estimate the difference between the two."),
            Difficulty::Hard
        );
    }

    #[test]
    fn test_attachment_is_hard() {
        assert_eq!(
            classify("Read the file. [Attached file: data.pdf]"),
            Difficulty::Hard
        );
    }

    #[test]
    fn test_long_is_hard() {
        let long = "x".repeat(LONG_TEXT_CHARS + 1);
        assert_eq!(classify(&long), Difficulty::Hard);
        let short = "x".repeat(10);
        assert_eq!(classify(&short), Difficulty::Simple);
    }

    #[test]
    fn test_hard_score_is_zero_on_trivial() {
        assert_eq!(hard_score("hello world"), 0);
        assert_eq!(hard_score("写个计划"), 0);
    }

    #[test]
    fn test_hard_score_accumulates() {
        // digit (10) + count verb (10) + ambiguity (5) + long (5)
        let mut t = "How many of the 42 samples at least match? ".to_string();
        t.push_str(&"detail ".repeat(60));
        assert!(hard_score(&t) >= 30);
    }

    #[test]
    fn test_cjk_question_is_hard_but_greeting_is_simple() {
        // Audit MEDIUM (2026-08-13): a CJK count question with no ASCII digit
        // was misclassified Simple, silently skipping verification.
        assert_eq!(classify("哪个国家人口最少？"), Difficulty::Hard);
        assert_eq!(classify("比较法国和日本的面积"), Difficulty::Hard);
        assert_eq!(classify("这个文件里有多少个数字？"), Difficulty::Hard);
        // Trivial greeting stays Simple.
        assert_eq!(classify("你好"), Difficulty::Simple);
        assert_eq!(classify("谢谢"), Difficulty::Simple);
    }
}

use everevo_core::context::{estimate_tokens, ContextBudget};

/// Clamp `content` so its estimated tokens stay within `budget_tokens`,
/// appending a truncation marker when trimmed.
///
/// `budget_tokens == 0` is a no-op (no cap configured). Uses binary search
/// over the char-prefix length for an exact fit against the CJK-aware
/// `estimate_tokens` heuristic.
pub(crate) fn clamp_content_by_tokens(content: &mut String, budget_tokens: usize) {
    if budget_tokens == 0 {
        return;
    }
    if estimate_tokens(content) <= budget_tokens {
        return;
    }
    let chars: Vec<char> = content.chars().collect();
    // Keep at least a meaningful prefix even when the budget is tiny.
    let mut lo = 64usize.min(chars.len());
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().collect();
        if estimate_tokens(&s) <= budget_tokens {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let truncated: String = chars[..lo].iter().collect();
    *content = format!("{truncated}\n… [truncated to fit context budget]");
}

/// Generous budget cap for the hard-question verification fragments
/// (AnswerDiscipline / VerifyCandidate / EvidenceChecklist).
///
/// `window == 0` (legacy) → no cap, unchanged behavior. Otherwise the shared
/// memory allocation is the bound — far larger than these prompts in practice,
/// so nothing truncates today, but worst-case growth can never starve the
/// conversation-history window.
pub(crate) fn clamp_verify_fragment(budget: &ContextBudget, content: &str) -> String {
    let mut s = content.to_string();
    let cap = if budget.window > 0 {
        budget.memory_budget
    } else {
        0
    };
    clamp_content_by_tokens(&mut s, cap);
    s
}
