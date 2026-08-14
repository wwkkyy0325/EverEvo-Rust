//! Deterministic final-answer sanitizer for the driver's commit path.
//!
//! The model sometimes violates the AnswerDiscipline contract and glues prose
//! onto the `Final answer:` value (48eb8242: `Final answer: 6Let me do a final
//! check...`), which breaks exact-match scoring even when the value is right.
//! This sanitizer applies TWO conservative rules that only fire on signatures
//! a legitimate single-line value never produces — so it can never corrupt a
//! correct answer:
//!
//! - **Rule B (glued numeric, same line):** after the last `Final answer:`
//!   marker, if the rest starts with a number immediately followed by a
//!   letter-word that is itself terminated by whitespace/punctuation, cut to
//!   the number. `6Let me` → `6`. Preserved: `3D`, `2nd`, `0.5mg`, `8, 29`,
//!   `1:41.614` (no letter immediately after the number, or the word runs to
//!   end-of-string without a terminator).
//! - **Rule A (trailing prose lines):** if the rest spans ≥2 non-empty lines,
//!   the FIRST line is a bare number, and a later line contains a letter, cut
//!   to the first line. `Final answer: 42\nLet me verify` → `42`. Preserved:
//!   `1:41.614\n...` (first line not a bare number), `8\n29\n22` (later lines
//!   have no letters).
//!
//! If neither rule fires, the text is returned UNCHANGED. The harness
//! (`scripts/gaia_bench.py`) mirrors these rules in `_glued_prose_guard`, with
//! a monotonic scorer fallback (belt-and-suspenders).

use regex::Regex;

/// Last `Final answer:` marker (mirrors the harness's extractor regex).
const MARKER_RE: &str = r"(?i)final\s+answer\s*(?:is\s*)?:?";

/// A number token: optional sign/$/%, digits with thousand-commas, optional
/// decimal, optional m³/m^3/m3/% suffix.
const NUMBER_TOKEN_RE: &str = r"[-+]?\$?\d[\d,]*(?:\.\d+)?(?:%|m3|m\^3|m³)?";

/// Rule B: number immediately glued to a letter-word that is itself followed
/// by a terminator (whitespace/punctuation). The terminator is what protects
/// `3D`, `2nd`, `0.5mg` (their trailing word runs to end-of-string).
const GLUE_RE: &str =
    r#"(?s)^\s*([-+]?\$?\d[\d,]*(?:\.\d+)?(?:%|m3|m\^3|m³)?)([A-Za-z][A-Za-z]*[\s.,;!?'"”])"#;

fn marker_end(text: &str) -> Option<usize> {
    let re = Regex::new(MARKER_RE).expect("MARKER_RE is static");
    re.find_iter(text).last().map(|m| m.end())
}

/// Cut a value out of the text after the last `Final answer:` marker, or
/// return `None` if no conservative rule fires.
fn cut_value(rest: &str) -> Option<String> {
    // Rule B — glued numeric on the same line.
    let glue = Regex::new(GLUE_RE).expect("GLUE_RE is static");
    if let Some(cap) = glue.captures(rest) {
        if let Some(value) = cap.get(1) {
            let v = value.as_str().trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // Rule A — first line is a bare number and a later line has letters.
    let bare = Regex::new(&format!(r"^{NUMBER_TOKEN_RE}$")).expect("bare-number RE is static");
    let lines: Vec<&str> = rest
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() >= 2 && bare.is_match(lines[0]) {
        let later_has_letters = lines[1..]
            .iter()
            .any(|l| l.chars().any(|c| c.is_ascii_alphabetic()));
        if later_has_letters {
            return Some(lines[0].to_string());
        }
    }
    None
}

/// Sanitize a committed final answer. Returns the original text unchanged
/// unless a conservative rule fired, in which case it returns a clean
/// `Final answer: <value>` line.
pub(crate) fn sanitize_final_answer(text: &str) -> String {
    let Some(end) = marker_end(text) else {
        return text.to_string();
    };
    let rest = &text[end..];
    match cut_value(rest) {
        Some(value) => format!("Final answer: {value}"),
        None => text.to_string(),
    }
}

/// Extract the bare value from a committed answer — the text after the last
/// `Final answer:` marker with the marker/colon/space stripped, or the whole
/// input when no marker is present.
pub(crate) fn final_answer_value(text: &str) -> String {
    let s = sanitize_final_answer(text);
    if let Some(idx) = s.to_lowercase().rfind("final answer") {
        let rest = s[idx + "final answer".len()..]
            .trim_start_matches([':', ' ', '\t', '=', '-']);
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    s
}

/// Deterministic content-anchored grounding check (2026-08-14).
///
/// A string/entity answer whose core content appears in NO retrieved tool
/// result is a memory fabrication — the commit-time substring check the
/// deterministic-guardrail research endorses ("every number / key term in the
/// claim must appear verbatim in a retrieved source"). Pure-numeric answers
/// are exempt here (an average is computed, not extracted) — those are the
/// SGV recompute gate's job. Returns `true` (don't block) when nothing
/// extractable survives the filters, so short uncheckable answers are never
/// false-blocked.
pub(crate) fn content_grounded(answer: &str, tool_texts: &[String]) -> bool {
    let pieces = groundable_pieces(answer);
    if pieces.is_empty() {
        return true;
    }
    let haystack = tool_texts.join(" ").to_lowercase();
    pieces
        .iter()
        .any(|p| haystack.contains(&p.to_lowercase()))
}

/// Extract the "groundable" content of a candidate answer — numeric runs
/// (digits with `.`/`,` separators, length ≥ 2) and alphabetic words of
/// length ≥ 5 (keeps `Lützow`, `Corps`, `Shannon`, `dastardly`; drops
/// stopwords, short particles, and single digits). `pub(crate)` for the
/// driver's computation-provenance gate.
pub(crate) fn groundable_pieces(answer: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for tok in answer.split(|c: char| !c.is_ascii_digit() && c != '.' && c != ',') {
        let t = tok.trim().trim_end_matches(['.', ',']);
        if t.chars().any(|c| c.is_ascii_digit()) && t.chars().count() >= 2 {
            pieces.push(t.to_string());
        }
    }
    for w in answer.split(|c: char| !(c.is_ascii_alphanumeric() || c == '\'' || c == '-')) {
        let w = w.trim_matches(|c| c == '-' || c == '\'');
        if w.chars().count() >= 5 && w.chars().any(|c| c.is_ascii_alphabetic()) {
            pieces.push(w.to_string());
        }
    }
    pieces.sort();
    pieces.dedup();
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glued_numeric_same_line_cut() {
        assert_eq!(
            sanitize_final_answer("Final answer: 6Let me do a final check..."),
            "Final answer: 6"
        );
    }

    #[test]
    fn trailing_prose_line_cut() {
        assert_eq!(
            sanitize_final_answer("Final answer: 42\nLet me verify this"),
            "Final answer: 42"
        );
    }

    #[test]
    fn preserved_multi_token_and_numeric_values() {
        for ok in [
            "Final answer: 1:41.614",
            "Final answer: Indonesia, Myanmar",
            "Final answer: 8, 29, 22, 1, 8, 26",
            "Final answer: bacon",
            "Final answer: The castle",
            "Final answer: 3D",
            "Final answer: 2nd",
            "Final answer: 0.5mg",
            "Final answer: 2023 in which the site changed",
            "Final answer: research",
        ] {
            assert_eq!(sanitize_final_answer(ok), ok, "must NOT corrupt: {ok}");
        }
    }

    #[test]
    fn no_marker_unchanged() {
        let s = "The answer is 6 in my view.";
        assert_eq!(sanitize_final_answer(s), s);
    }

    #[test]
    fn marker_with_is_variant() {
        assert_eq!(
            sanitize_final_answer("Final answer is: 6Let me check"),
            "Final answer: 6"
        );
    }

    #[test]
    fn numeric_list_on_own_line_preserved() {
        // Multi-line numeric list, later lines have no letters → no cut.
        let s = "Final answer: 8\n29\n22";
        assert_eq!(sanitize_final_answer(s), s);
    }

    #[test]
    fn final_answer_value_extracts_bare_value() {
        assert_eq!(
            final_answer_value("Final answer: Lützow Free Corps"),
            "Lützow Free Corps"
        );
        assert_eq!(final_answer_value("Final answer: 26.4"), "26.4");
        assert_eq!(final_answer_value("no marker here"), "no marker here");
    }

    #[test]
    fn content_grounded_requires_source_match() {
        // Fully-fabricated entity → NOT grounded.
        let tools = ["A biography of the author, who later joined the Russian-German Legion.".to_string()];
        assert!(!content_grounded("Lützow Free Corps", &tools));
        // Term present → grounded.
        let tools2 = ["He served with the Lützow Free Corps before emigrating.".to_string()];
        assert!(content_grounded("Lützow Free Corps", &tools2));
        // Short phrase with no groundable token → never blocked.
        assert!(content_grounded("So we had to let it die.", &[]));
        // Number matching.
        let tools3 = ["The total came to 776 views in the second column.".to_string()];
        assert!(content_grounded("776, 665", &tools3));
        assert!(!content_grounded("8058", &tools3));
    }
}
