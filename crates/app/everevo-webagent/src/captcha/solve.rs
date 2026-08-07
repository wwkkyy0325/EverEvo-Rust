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

// ── Vision AI solver ─────────────────────────────────────────────────────

/// Vision-based CAPTCHA solver using Claude Vision / GPT-4V / Qwen-VL.
///
/// Reads API credentials from environment variables:
/// - `EVEREVO_VISION_PROVIDER`: `"openai"` or `"anthropic"` (default: `"openai"`)
/// - `OPENAI_API_KEY`, `OPENAI_MODEL` (default: `gpt-4o`), `OPENAI_BASE_URL`
/// - `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` (default: `claude-sonnet-4-5-20250929`), `ANTHROPIC_BASE_URL`
pub struct VisionCaptchaSolver {
    api_format: String,
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl VisionCaptchaSolver {
    /// Create a VisionCaptchaSolver from environment variables.
    /// Falls back gracefully if no vision API credentials are configured.
    pub fn from_env() -> Option<Self> {
        let provider = std::env::var("EVEREVO_VISION_PROVIDER")
            .unwrap_or_else(|_| "openai".into())
            .to_lowercase();

        let (api_key, model, base_url) = if provider == "anthropic" {
            (
                std::env::var("ANTHROPIC_API_KEY").ok()?,
                std::env::var("ANTHROPIC_MODEL")
                    .unwrap_or_else(|_| "claude-sonnet-4-5-20250929".into()),
                std::env::var("ANTHROPIC_BASE_URL")
                    .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            )
        } else {
            (
                std::env::var("OPENAI_API_KEY").ok()?,
                std::env::var("OPENAI_MODEL")
                    .unwrap_or_else(|_| "gpt-4o".into()),
                std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com".into()),
            )
        };

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .ok()?;

        Some(Self {
            api_format: provider,
            api_key,
            base_url,
            model,
            client,
        })
    }

    /// Send a screenshot to the vision API and get structured click coordinates back.
    async fn vision_solve(
        &self,
        screenshot: &[u8],
        instruction: &str,
    ) -> Result<CaptchaSolution, String> {
        // Simple base64 encode (no external dep needed)
        const BASE64_TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut base64_img = String::with_capacity(screenshot.len() * 4 / 3 + 4);
        for chunk in screenshot.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            base64_img.push(BASE64_TABLE[((triple >> 18) & 0x3F) as usize] as char);
            base64_img.push(BASE64_TABLE[((triple >> 12) & 0x3F) as usize] as char);
            base64_img.push(if chunk.len() > 1 { BASE64_TABLE[((triple >> 6) & 0x3F) as usize] as char } else { '=' });
            base64_img.push(if chunk.len() > 2 { BASE64_TABLE[(triple & 0x3F) as usize] as char } else { '=' });
        }
        let prompt = format!(
            "You are solving a CAPTCHA. The instruction is: \"{instruction}\"\n\n\
             Analyze the image and return ONLY a JSON object with the solution. Format:\n\
             - For image grid (click tiles): {{\"clicks\": [[x1,y1], [x2,y2], ...]}} where coordinates are pixel positions of tile centers\n\
             - For text CAPTCHA: {{\"text\": \"the text\"}}\n\
             - For slider: {{\"slide_offset\": number}} where number is pixels to drag right\n\
             - If unsolvable: {{\"unsolvable\": \"reason\"}}\n\n\
             Respond with ONLY the JSON, no other text."
        );

        match self.api_format.as_str() {
            "anthropic" => {
                let body = serde_json::json!({
                    "model": self.model,
                    "max_tokens": 256,
                    "messages": [{
                        "role": "user",
                        "content": [
                            {"type": "text", "text": prompt},
                            {"type": "image", "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": base64_img
                            }}
                        ]
                    }]
                });
                let resp = self
                    .client
                    .post(format!("{}/v1/messages", self.base_url))
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Vision API request failed: {e}"))?;
                let json: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("Vision API parse: {e}"))?;
                let text = json["content"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                Self::parse_vision_response(&text)
            }
            _ => {
                // OpenAI-compatible format (GPT-4V, GPT-4o, Qwen-VL, etc.)
                let body = serde_json::json!({
                    "model": self.model,
                    "max_tokens": 256,
                    "messages": [{
                        "role": "user",
                        "content": [
                            {"type": "text", "text": prompt},
                            {"type": "image_url", "image_url": {
                                "url": format!("data:image/png;base64,{}", base64_img)
                            }}
                        ]
                    }]
                });
                let resp = self
                    .client
                    .post(format!("{}/v1/chat/completions", self.base_url))
                    .header("Authorization", format!("Bearer {}", self.api_key))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Vision API request failed: {e}"))?;
                let json: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("Vision API parse: {e}"))?;
                let text = json["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                Self::parse_vision_response(&text)
            }
        }
    }

    /// Parse the vision model's JSON response into a CaptchaSolution.
    fn parse_vision_response(text: &str) -> Result<CaptchaSolution, String> {
        // Strip code fences if present
        let clean = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let json: serde_json::Value =
            serde_json::from_str(clean).map_err(|e| format!("Parse vision response: {e} — raw: {text}"))?;

        if let Some(reason) = json.get("unsolvable").and_then(|v| v.as_str()) {
            return Ok(CaptchaSolution::Unsolvable(reason.into()));
        }
        if let Some(clicks) = json.get("clicks").and_then(|v| v.as_array()) {
            let points: Vec<(f64, f64)> = clicks
                .iter()
                .filter_map(|c| {
                    let arr = c.as_array()?;
                    Some((arr.first()?.as_f64()?, arr.get(1)?.as_f64()?))
                })
                .collect();
            if !points.is_empty() {
                return Ok(CaptchaSolution::ClickPoints(points));
            }
        }
        if let Some(text_val) = json.get("text").and_then(|v| v.as_str()) {
            return Ok(CaptchaSolution::Text(text_val.into()));
        }
        if let Some(offset) = json.get("slide_offset").and_then(|v| v.as_f64()) {
            return Ok(CaptchaSolution::SlideOffset(offset));
        }

        Err(format!("Unrecognized vision response format: {text}"))
    }
}

#[async_trait]
impl CaptchaSolver for VisionCaptchaSolver {
    async fn solve(
        &self,
        screenshot: Option<Vec<u8>>,
        challenge_type: &ChallengeType,
        instruction: &str,
        page_html: &str,
    ) -> CaptchaSolution {
        // Delegate non-visual types to the simple solver
        match challenge_type {
            ChallengeType::Turnstile
            | ChallengeType::RecaptchaV2Checkbox
            | ChallengeType::RecaptchaV3 => {
                return SimpleCaptchaSolver
                    .solve(None, challenge_type, instruction, page_html)
                    .await;
            }
            _ => {}
        }

        // For visual challenges, we need a screenshot
        let img = match screenshot {
            Some(ref bytes) if !bytes.is_empty() => bytes.as_slice(),
            _ => {
                return CaptchaSolution::Unsolvable(
                    "VisionCaptchaSolver requires a screenshot. \
                     Ensure CDP Page.captureScreenshot is called before solving."
                        .into(),
                );
            }
        };

        match self.vision_solve(img, instruction).await {
            Ok(solution) => solution,
            Err(e) => {
                tracing::warn!(error = %e, "VisionCaptchaSolver failed, falling back");
                // Fall back to the simple solver's Unsolvable for this type
                SimpleCaptchaSolver
                    .solve(None, challenge_type, instruction, page_html)
                    .await
            }
        }
    }
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
