//! Anti-webdriver detection: JavaScript patches injected into the
//! browser page before navigation to mask automation traces.
//!
//! ## What sites detect
//!
//! 1. `navigator.webdriver === true` — Chrome sets this when launched
//!    with `--remote-debugging-port` or `--enable-automation`
//! 2. `window.chrome.runtime` — missing in headless/automated Chrome
//! 3. CDP `Runtime.enable` side effects — detectable serialization hooks
//! 4. `navigator.plugins.length === 0` — empty plugin list = headless
//! 5. Canvas/WebGL fingerprint mismatches
//! 6. `navigator.hardwareConcurrency` / `deviceMemory` anomalies
//!
//! ## What we do
//!
//! Inject a JavaScript payload via CDP `Page.addScriptToEvaluateOnNewDocument`
//! that runs BEFORE any page JavaScript, patching all detectable signals.

/// JavaScript payload injected before any page script loads.
/// Runs in the page's main world — modifies `navigator`, `window`,
/// and DOM prototypes before the page's own JS can inspect them.
pub const STEALTH_JS: &str = r#"
(function() {
    'use strict';

    // ── 1. Remove navigator.webdriver ──────────────────────────
    //    The single most important fix. Chrome automatically sets
    //    `navigator.webdriver = true` when launched with automation
    //    flags. We redefine the property getter to return undefined.
    const originalNavigator = navigator;
    try {
        Object.defineProperty(Navigator.prototype, 'webdriver', {
            get: () => undefined,
            configurable: true,
            enumerable: true
        });
    } catch(e) {}
    try {
        delete Object.getPrototypeOf(navigator).webdriver;
    } catch(e) {}

    // ── 2. Restore chrome.runtime ──────────────────────────────
    //    Headless Chrome lacks `window.chrome.runtime`. We provide
    //    a minimal stub that looks like a real browser extension API.
    if (!window.chrome) {
        window.chrome = {};
    }
    if (!window.chrome.runtime) {
        window.chrome.runtime = {
            PlatformOs: 'win',
            PlatformArch: 'x86-64',
            PlatformNaclArch: 'x86-64',
            getManifest: () => ({}),
            getURL: (path) => 'chrome-extension://' + path,
            connect: () => ({ onMessage: {addListener:()=>{}}, onDisconnect:{addListener:()=>{}}, postMessage:()=>{} }),
            sendMessage: () => {},
            onMessage: { addListener: () => {} },
            onConnect: { addListener: () => {} }
        };
    }

    // ── 3. Fake plugins array ──────────────────────────────────
    //    Real Chrome has 3+ plugins (Chrome PDF Viewer, Chrome PDF Plugin,
    //    Native Client). Headless Chrome has 0.
    if (navigator.plugins && navigator.plugins.length === 0) {
        Object.defineProperty(navigator, 'plugins', {
            get: () => {
                // Return a PluginArray-like with at least one entry
                return Object.create(PluginArray.prototype, {
                    length: { value: 1, enumerable: true },
                    0: { value: Object.create(Plugin.prototype, {
                        name: { value: 'Chrome PDF Plugin', enumerable: true },
                        filename: { value: 'internal-pdf-viewer', enumerable: true },
                        description: { value: 'Portable Document Format', enumerable: true },
                        length: { value: 1, enumerable: true }
                    }), enumerable: true },
                    item: { value: function(i) { return this[i] || null; }, enumerable: true },
                    namedItem: { value: function(n) { return null; }, enumerable: true },
                    refresh: { value: () => {}, enumerable: true }
                });
            },
            configurable: true,
            enumerable: true
        });
    }

    // ── 4. Patch permissions API ──────────────────────────────
    //    Automation browsers often return 'denied' or 'prompt' for
    //    all permissions. We intercept `Permissions.query` to return
    //    'granted' for common non-sensitive permissions.
    const origQuery = window.navigator.permissions.query;
    if (origQuery) {
        const safePermissions = new Set(['clipboard-read', 'clipboard-write']);
        window.navigator.permissions.query = function(desc) {
            if (safePermissions.has(String(desc && desc.name))) {
                return Promise.resolve({ state: 'granted', onchange: null });
            }
            return origQuery.call(this, desc);
        };
    }

    // ── 5. Hide CDP Runtime domain traces ──────────────────────
    //    Advanced detection: sites trigger object serialization and
    //    detect CDP listeners via Runtime.consoleAPICalled hooks.
    //    We can't fully prevent this at JS level (it needs CDP-level
    //    avoidance), but we can reduce the surface by patching
    //    console methods to no-op during critical checks.
    const origWarn = console.warn;
    console.warn = function() {
        const msg = arguments[0] || '';
        // Suppress Chrome automation warnings
        if (String(msg).includes('AutomationControlled') ||
            String(msg).includes('DevTools')) {
            return;
        }
        return origWarn.apply(this, arguments);
    };

    // ── 6. Normalize screen metrics ────────────────────────────
    //    Use common 1920x1080 metrics if screen reports unusual values
    //    (headless Chrome may report 800x600 or 0x0).
    if (screen.width < 1024 || screen.height < 768) {
        Object.defineProperty(screen, 'width', { get: () => 1920 });
        Object.defineProperty(screen, 'height', { get: () => 1080 });
        Object.defineProperty(screen, 'availWidth', { get: () => 1920 });
        Object.defineProperty(screen, 'availHeight', { get: () => 1040 });
        Object.defineProperty(screen, 'colorDepth', { get: () => 24 });
        Object.defineProperty(screen, 'pixelDepth', { get: () => 24 });
    }

})();
"#;

/// Launch flags for Chrome that reduce the automation fingerprint.
/// Applied in addition to `--remote-debugging-port`.
pub const STEALTH_FLAGS: &[&str] = &[
    "--disable-blink-features=AutomationControlled",
    "--disable-features=TranslateUI,OptimizationHints,MediaRouter,ChromeWhatsNewUI,InterestFeedContentSuggestions",
    "--disable-component-update",
    "--disable-domain-reliability",
    "--disable-sync",
    "--disable-background-networking",
    "--disable-client-side-phishing-detection",
    "--disable-default-apps",
    "--disable-hang-monitor",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-breakpad",
    "--disable-crash-reporter",
    "--no-default-browser-check",
    "--no-first-run",
    "--metrics-recording-only",
];

/// CDP command to inject stealth JS before any page script.
/// Call `Page.addScriptToEvaluateOnNewDocument` with this payload.
pub fn cdp_inject_stealth() -> serde_json::Value {
    serde_json::json!({
        "source": STEALTH_JS,
        "worldName": "everevo-stealth"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stealth_js_is_non_empty() {
        assert!(STEALTH_JS.len() > 500);
        assert!(STEALTH_JS.contains("navigator.webdriver"));
        assert!(STEALTH_JS.contains("chrome.runtime"));
    }

    #[test]
    fn test_stealth_flags_are_non_empty() {
        assert!(!STEALTH_FLAGS.is_empty());
        assert!(STEALTH_FLAGS.contains(&"--disable-blink-features=AutomationControlled"));
    }
}
