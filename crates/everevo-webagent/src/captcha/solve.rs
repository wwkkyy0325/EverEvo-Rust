//! CAPTCHA solving — trait-based architecture with built-in simple solver
//! and pluggable vision-AI backend for image-based challenges.
//!
//! ## Architecture
//!
//! ```
//! CDP browser detects challenge → detect_challenge() → CaptchaSolver
//!   ├── SimpleCaptchaSolver (checkbox click, Turnstile wait)
//!   └── VisionCaptchaSolver (future: GPT-4V / Claude Vision / Qwen-VL)
//!       └── takes screenshot → vision model → click coordinates
//! ```
//!
//! ## Pluggable vision model interface
//!
//! The `CaptchaSolver` trait accepts raw screenshot bytes + challenge metadata
//! and returns structured actions. Any vision-capable model can implement it.
//! The default `SimpleCaptchaSolver` handles checkbox/Turnstile without AI;
//! image-grid challenges return `Unsolvable` with a clear reason.

use async_trait::async_trait;

use super::detect::ChallengeType;

// ── Solution types ──────────────────────────────────────────────────────

/// Action to perform on the CAPTCHA element.
#[derive(Debug, Clone)]
pub enum CaptchaSolution {
    /// Click at these (x, y) pixel coordinates relative to the challenge iframe.
    /// For image-grid challenges: one click per matching tile.
    ClickPoints(Vec<(f64, f64)>),
    /// Type this text into the input field.
    Text(String),
    /// Drag the slider by this many pixels to the right.
    SlideOffset(f64),
    /// Simply wait — the challenge auto-resolves (Turnstile, v3).
    WaitAndRetry,
    /// Cannot solve automatically. `reason` explains why.
    Unsolvable(String),
}

// ── Solver trait ────────────────────────────────────────────────────────

/// A CAPTCHA solver — receives a screenshot + challenge metadata and
/// returns the action to perform.
///
/// Implementations:
/// - `SimpleCaptchaSolver` — checkbox, Turnstile (no AI needed)
/// - (future) `VisionCaptchaSolver` — GPT-4V / Claude Vision / Qwen-VL
#[async_trait]
pub trait CaptchaSolver: Send + Sync {
    /// Attempt to solve the challenge.
    ///
    /// - `screenshot`: PNG image bytes of the challenge area (or full page)
    /// - `challenge_type`: what kind of CAPTCHA was detected
    /// - `instruction`: the human-readable prompt ("Select all crosswalks")
    /// - `page_html`: full rendered HTML for context
    async fn solve(
        &self,
        screenshot: Option<Vec<u8>>,
        challenge_type: &ChallengeType,
        instruction: &str,
        page_html: &str,
    ) -> CaptchaSolution;
}

// ── Simple solver (no AI) ───────────────────────────────────────────────

/// Handles challenges that don't require vision AI:
/// - Cloudflare Turnstile → wait
/// - reCAPTCHA v2 checkbox → click
/// - reCAPTCHA v3 → good behavior
///
/// Image-grid and slider challenges return `Unsolvable`.
pub struct SimpleCaptchaSolver;

#[async_trait]
impl CaptchaSolver for SimpleCaptchaSolver {
    async fn solve(
        &self,
        _screenshot: Option<Vec<u8>>,
        challenge_type: &ChallengeType,
        _instruction: &str,
        page_html: &str,
    ) -> CaptchaSolution {
        match challenge_type {
            ChallengeType::Turnstile => {
                // Cloudflare Turnstile auto-passes after JS execution.
                // Just wait a few seconds and retry.
                CaptchaSolution::WaitAndRetry
            }
            ChallengeType::RecaptchaV2Checkbox => {
                // The checkbox is inside a reCAPTCHA iframe.
                // Click at the center of the checkbox element.
                // CDP: document.querySelector('.recaptcha-checkbox').click()
                CaptchaSolution::ClickPoints(vec![(0.0, 0.0)]) // center click — caller handles CDP coordinates
            }
            ChallengeType::RecaptchaV3 => {
                // v3 is invisible and score-based. Good browser behavior
                // (human-like timing, mouse movements) maintains a high score.
                CaptchaSolution::WaitAndRetry
            }
            ChallengeType::TextCaptcha => {
                // Try to extract CAPTCHA text from the DOM.
                // Many text CAPTCHAs embed hints in alt text, labels,
                // or data attributes for accessibility compliance.
                if let Some(text) = extract_captcha_text_from_dom(page_html) {
                    CaptchaSolution::Text(text)
                } else {
                    CaptchaSolution::Unsolvable(
                        "Text CAPTCHA detected but text could not be extracted from DOM. \
                         Needs vision AI for image-based text recognition."
                            .into(),
                    )
                }
            }
            ChallengeType::RecaptchaV2Grid => {
                CaptchaSolution::Unsolvable(
                    "reCAPTCHA v2 image grid requires a vision AI model \
                     (GPT-4V, Claude Vision, or Qwen-VL). \
                     Implement VisionCaptchaSolver to handle this."
                        .into(),
                )
            }
            ChallengeType::HCaptcha => {
                CaptchaSolution::Unsolvable(
                    "hCaptcha image selection requires a vision AI model. \
                     Implement VisionCaptchaSolver to handle this."
                        .into(),
                )
            }
            ChallengeType::Slider => {
                CaptchaSolution::Unsolvable(
                    "Slider CAPTCHA requires a vision AI model to compute \
                     the slide offset. Implement VisionCaptchaSolver."
                        .into(),
                )
            }
            ChallengeType::Unknown(desc) => {
                CaptchaSolution::Unsolvable(format!(
                    "Unknown challenge type: {desc}. Screenshot may be needed \
                     for manual review or vision model analysis."
                ))
            }
        }
    }
}

/// Try to extract readable CAPTCHA text from DOM markup.
///
/// Many sites embed hints in `<img alt="...">`, hidden `<label>` elements,
/// or data attributes for accessibility compliance.
fn extract_captcha_text_from_dom(html: &str) -> Option<String> {
    // Pattern 1: <img ... alt="captcha text">
    for alt_pat in &["alt=\"", "alt='"] {
        if let Some(pos) = html.to_lowercase().find("captcha") {
            // Search backward from the captcha mention for an alt attr
            if let Some(alt_start) = html[..pos].rfind(alt_pat) {
                let val_start = alt_start + alt_pat.len();
                let quote = if alt_pat.ends_with('"') { '"' } else { '\'' };
                if let Some(val_end) = html[val_start..].find(quote) {
                    let text = &html[val_start..val_start + val_end];
                    if text.len() >= 3 && text.len() <= 20 && !text.contains('<') {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }

    // Pattern 2: <input name="captcha" value="..."> — sometimes the value contains hints
    // Pattern 3: Text directly after "captcha" label
    if let Some(pos) = html.to_lowercase().find("captcha") {
        let nearby = &html[pos..];
        // Look for a short alphanumeric string nearby
        for word in nearby.split(|c: char| !c.is_alphanumeric()) {
            if word.len() >= 3 && word.len() <= 10 && word.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some(word.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captcha::detect::ChallengeType;

    #[tokio::test]
    async fn test_simple_solver_turnstile() {
        let solver = SimpleCaptchaSolver;
        let result = solver
            .solve(None, &ChallengeType::Turnstile, "", "")
            .await;
        assert!(matches!(result, CaptchaSolution::WaitAndRetry));
    }

    #[tokio::test]
    async fn test_simple_solver_checkbox() {
        let solver = SimpleCaptchaSolver;
        let result = solver
            .solve(None, &ChallengeType::RecaptchaV2Checkbox, "", "")
            .await;
        assert!(matches!(result, CaptchaSolution::ClickPoints(_)));
    }

    #[tokio::test]
    async fn test_simple_solver_grid_is_unsolvable() {
        let solver = SimpleCaptchaSolver;
        let result = solver
            .solve(None, &ChallengeType::RecaptchaV2Grid, "Select all crosswalks", "")
            .await;
        assert!(matches!(result, CaptchaSolution::Unsolvable(_)));
    }
}
