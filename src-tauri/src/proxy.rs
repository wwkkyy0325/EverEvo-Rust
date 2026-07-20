//! Proxy layer — bypasses Tauri WebView CORS restrictions.
//!
//! In production, the frontend is loaded from `tauri://localhost` but the
//! backend runs on `http://127.0.0.1:3000`. WebView blocks cross-origin
//! requests between these origins. The proxy intercepts Tauri webview
//! navigation requests and forwards them to the backend.
//!
//! For now, we handle this via Tauri commands (commands.rs) rather than an
//! HTTP proxy — the frontend calls Tauri commands, which forward to the
//! Axum backend internally. This avoids CORS entirely.

use tauri::UriSchemeResponder;

/// Handle custom protocol `everevo://` for WebView resource loading.
/// Currently a stub — used if we want to serve the frontend directly.
#[allow(dead_code)]
pub fn handle_everevo_protocol(_request: String, _responder: UriSchemeResponder) {
    // Future: serve static frontend assets via everevo:// protocol
}
