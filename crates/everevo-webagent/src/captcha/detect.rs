//! CAPTCHA type detection — identifies which challenge system is active
//! on a page before deciding how to solve it.
//!
//! Public API will be wired into MCP browser tools in Phase B/C.

/// Known CAPTCHA/challenge types.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ChallengeType {
    /// Cloudflare Turnstile — auto-passes with good browser behavior.
    Turnstile,
    /// reCAPTCHA v2 checkbox ("I'm not a robot") — clickable.
    RecaptchaV2Checkbox,
    /// reCAPTCHA v2 image grid — requires vision AI.
    RecaptchaV2Grid,
    /// reCAPTCHA v3 — invisible, score-based. Good behavior avoids it.
    RecaptchaV3,
    /// hCaptcha image selection — requires vision AI.
    HCaptcha,
    /// Simple text-based CAPTCHA — OCR-solvable.
    TextCaptcha,
    /// Slider/puzzle CAPTCHA — requires vision AI.
    Slider,
    /// Unknown challenge — screenshot + report to user.
    Unknown(String),
}

/// Detect the CAPTCHA type from the page HTML + DOM state.
///
/// Called after JavaScript has rendered the page.
/// `html` is the full rendered DOM, `title` is `document.title`.
pub fn detect_challenge(html: &str, title: &str) -> Option<ChallengeType> {
    let h = html;

    // Cloudflare Turnstile
    if h.contains("cf-turnstile")
        || h.contains("challenge-platform")
        || h.contains("turnstile")
        || title.contains("Just a moment")
    {
        return Some(ChallengeType::Turnstile);
    }

    // reCAPTCHA v2 image grid (after checkbox click, the grid appears)
    if (h.contains("recaptcha") || h.contains("g-recaptcha"))
        && (h.contains("imageselect") || h.contains("tile") || h.contains("Select all images"))
    {
        return Some(ChallengeType::RecaptchaV2Grid);
    }

    // reCAPTCHA v2 checkbox
    if h.contains("recaptcha")
        || h.contains("g-recaptcha")
        || h.contains("recaptcha-checkbox")
    {
        return Some(ChallengeType::RecaptchaV2Checkbox);
    }

    // hCaptcha
    if h.contains("hcaptcha") || h.contains("h-captcha") {
        return Some(ChallengeType::HCaptcha);
    }

    // Text-based CAPTCHA (look for common patterns)
    if h.contains("captcha-image")
        || (title.to_lowercase().contains("captcha") && h.contains("<img"))
    {
        return Some(ChallengeType::TextCaptcha);
    }

    // Slider detection
    if h.contains("sliderCaptcha")
        || h.contains("nc_wrapper")
        || h.contains("slideVerify")
        || h.contains("dragVerify")
    {
        return Some(ChallengeType::Slider);
    }

    // Generic challenge page
    if is_challenge_page(h) {
        return Some(ChallengeType::Unknown("generic_bot_detection".into()));
    }

    None
}

/// Quick check: is this HTML likely a challenge/block page?
fn is_challenge_page(html: &str) -> bool {
    html.contains("anomaly.js")
        || html.contains("challenge-form")
        || html.contains("ddg_ptoken")
        || html.contains("captcha-delivery.com")
        || html.contains("tr.bing.com")
        || html.contains("Just a moment...")
        || html.contains("Checking your browser")
        || html.contains("cf-browser-verification")
        || html.contains("_cf_chl_opt")
        || html.contains("Please turn JavaScript on")
        || html.contains("Please enable JavaScript")
        || html.contains("Attention Required! | Cloudflare")
        || html.contains("DDoS protection")
        || (html.len() < 200 && !html.contains("<a "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_turnstile() {
        let html = r#"<div class="cf-turnstile"></div><p>Just a moment</p>"#;
        assert_eq!(
            detect_challenge(html, "Just a moment..."),
            Some(ChallengeType::Turnstile)
        );
    }

    #[test]
    fn test_detect_recaptcha_v2_checkbox() {
        let html = r#"<div class="g-recaptcha" data-sitekey="xxx"></div>"#;
        assert_eq!(
            detect_challenge(html, "Example Page"),
            Some(ChallengeType::RecaptchaV2Checkbox)
        );
    }

    #[test]
    fn test_detect_recaptcha_v2_grid() {
        let html = r#"<div class="g-recaptcha"><div class="imageselect">Select all crosswalks</div></div>"#;
        assert_eq!(
            detect_challenge(html, "reCAPTCHA"),
            Some(ChallengeType::RecaptchaV2Grid)
        );
    }

    #[test]
    fn test_no_challenge_on_normal_page() {
        // Realistic search result page — long enough to not trigger
        // the "empty error page" heuristic.
        let html = r#"<html><body>
            <h1>Search Results</h1>
            <div class="results">
                <a href="https://example.com">Example</a>
                <p>Some normal content that is long enough to be >200 chars.
                Additional text to make sure the page looks like a real
                search result page rather than an error or block page.</p>
            </div>
        </body></html>"#;
        assert_eq!(detect_challenge(html, "Search Results"), None);
    }
}
