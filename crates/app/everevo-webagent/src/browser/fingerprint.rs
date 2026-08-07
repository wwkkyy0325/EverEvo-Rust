//! Coherent browser fingerprint builder.
//!
//! ## Why not randomize?
//!
//! Randomizing individual fingerprint attributes creates inconsistencies
//! that anti-bot systems detect. A fingerprint must be **internally
//! consistent**: OS, browser version, screen resolution, GPU, fonts,
//! timezone, and language must all agree.
//!
//! This module selects a **coherent identity** from a set of real-world
//! profiles and applies all attributes together.

/// A complete, coherent browser fingerprint profile.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Fingerprint {
    pub user_agent: &'static str,
    pub platform: &'static str,
    pub vendor: &'static str,
    pub screen_width: u32,
    pub screen_height: u32,
    pub avail_width: u32,
    pub avail_height: u32,
    pub color_depth: u32,
    pub pixel_depth: u32,
    pub device_memory: u32,     // GB
    pub hardware_concurrency: u32,
    pub timezone: &'static str,
    pub language: &'static str,
    pub languages: &'static [&'static str],
}

/// Pre-built profiles matching real browser configurations.
const PROFILES: &[Fingerprint] = &[
    // Windows 11 + Chrome 131 + 1920x1080 (most common)
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        platform: "Win32",
        vendor: "Google Inc.",
        screen_width: 1920, screen_height: 1080,
        avail_width: 1920, avail_height: 1040,
        color_depth: 24, pixel_depth: 24,
        device_memory: 8, hardware_concurrency: 16,
        timezone: "Asia/Shanghai",
        language: "zh-CN",
        languages: &["zh-CN", "zh", "en-US", "en"],
    },
    // Windows 11 + Edge 131 + 2560x1440
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
        platform: "Win32",
        vendor: "Google Inc.",
        screen_width: 2560, screen_height: 1440,
        avail_width: 2560, avail_height: 1400,
        color_depth: 24, pixel_depth: 24,
        device_memory: 16, hardware_concurrency: 20,
        timezone: "Asia/Shanghai",
        language: "zh-CN",
        languages: &["zh-CN", "zh", "en-US", "en"],
    },
    // macOS + Chrome 131 + 1680x1050
    Fingerprint {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        platform: "MacIntel",
        vendor: "Google Inc.",
        screen_width: 1680, screen_height: 1050,
        avail_width: 1680, avail_height: 978,
        color_depth: 24, pixel_depth: 24,
        device_memory: 16, hardware_concurrency: 12,
        timezone: "America/Los_Angeles",
        language: "en-US",
        languages: &["en-US", "en"],
    },
    // Windows 10 + Chrome 131 + 1366x768 (laptop)
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        platform: "Win32",
        vendor: "Google Inc.",
        screen_width: 1366, screen_height: 768,
        avail_width: 1366, avail_height: 728,
        color_depth: 24, pixel_depth: 24,
        device_memory: 4, hardware_concurrency: 8,
        timezone: "Asia/Shanghai",
        language: "zh-CN",
        languages: &["zh-CN", "zh", "en"],
    },
];

/// Select a coherent fingerprint profile.
///
/// Rotates deterministically based on a seed (e.g., PID or random).
/// Returns one of the pre-built profiles — all attributes are internally
/// consistent and match real-world browser configurations.
pub fn select_fingerprint(seed: u64) -> &'static Fingerprint {
    &PROFILES[seed as usize % PROFILES.len()]
}

/// Build the `Page.addScriptToEvaluateOnNewDocument` payload that
/// overrides browser fingerprint attributes to match the selected profile.
pub fn fingerprint_injection_js(fp: &Fingerprint) -> String {
    format!(
        r#"(function(){{
  // Override navigator properties
  Object.defineProperty(navigator, 'platform', {{get:()=>"{}"}});
  Object.defineProperty(navigator, 'vendor', {{get:()=>"{}"}});
  Object.defineProperty(navigator, 'language', {{get:()=>"{}"}});
  Object.defineProperty(navigator, 'languages', {{get:()=>[{}]}});
  Object.defineProperty(navigator, 'hardwareConcurrency', {{get:()=>{}}});
  Object.defineProperty(navigator, 'deviceMemory', {{get:()=>{}}});

  // Override screen properties
  Object.defineProperty(screen, 'width', {{get:()=>{}}});
  Object.defineProperty(screen, 'height', {{get:()=>{}}});
  Object.defineProperty(screen, 'availWidth', {{get:()=>{}}});
  Object.defineProperty(screen, 'availHeight', {{get:()=>{}}});
  Object.defineProperty(screen, 'colorDepth', {{get:()=>{}}});
  Object.defineProperty(screen, 'pixelDepth', {{get:()=>{}}});

  // Override timezone
  try {{
    const orig = Intl.DateTimeFormat().resolvedOptions();
    Intl.DateTimeFormat = function(locales, opts) {{
      const df = new orig.constructor(locales, opts);
      const origResolved = df.resolvedOptions.bind(df);
      df.resolvedOptions = function() {{
        const r = origResolved();
        r.timeZone = "{}";
        return r;
      }};
      return df;
    }};
  }} catch(e) {{}}
}})()"#,
        fp.platform, fp.vendor,
        fp.language,
        fp.languages.iter().map(|l| format!("\"{l}\"")).collect::<Vec<_>>().join(","),
        fp.hardware_concurrency, fp.device_memory,
        fp.screen_width, fp.screen_height,
        fp.avail_width, fp.avail_height,
        fp.color_depth, fp.pixel_depth,
        fp.timezone,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profiles_consistent() {
        for (i, fp) in PROFILES.iter().enumerate() {
            assert!(!fp.user_agent.is_empty(), "profile {i}: empty UA");
            assert!(fp.screen_width >= fp.avail_width, "profile {i}: invalid screen/avail");
            assert!(fp.languages.len() >= 1, "profile {i}: no languages");
        }
    }

    #[test]
    fn test_select_deterministic() {
        let fp1 = select_fingerprint(42);
        let fp2 = select_fingerprint(42);
        assert_eq!(fp1.user_agent, fp2.user_agent);
    }

    #[test]
    fn test_injection_js_valid() {
        let fp = select_fingerprint(0);
        let js = fingerprint_injection_js(fp);
        assert!(js.contains("navigator"), "should contain navigator");
        assert!(js.contains("screen"), "should contain screen");
        assert!(js.contains("defineProperty"), "should use defineProperty");
    }
}
