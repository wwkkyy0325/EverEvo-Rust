//! Chat endpoint — session-aware SSE streaming with context pipeline.
//!
//! Split across modules:
//! - `handler` — main request handler + SSE + agent loop
//! - `slash_commands` — /character, /plan, /workspace handlers
//! - `post_turn` — background tasks spawned after each turn
//! - `helpers` — title truncation, DB conversion, permission, git, workspace context
//! - `reconnect` — session reconnection handler

pub mod auto_continue;
pub mod handler;
pub mod helpers;
pub mod post_turn;
pub mod reconnect;
pub mod slash_commands;
pub mod wiring;

pub use handler::router;

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::helpers::{resolve_permission, truncate_for_title};

    #[test]
    fn test_truncate_short_text() {
        assert_eq!(truncate_for_title("hello"), "hello");
    }

    #[test]
    fn test_truncate_trim_and_first_line() {
        assert_eq!(truncate_for_title("hello\nworld"), "hello");
    }

    #[test]
    fn test_truncate_long_text() {
        let long = "a".repeat(100);
        let result = truncate_for_title(&long);
        assert!(result.len() <= 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_exactly_60() {
        let exact = "a".repeat(60);
        assert_eq!(truncate_for_title(&exact), exact);
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_for_title(""), "");
    }

    #[test]
    fn test_resolve_permission_known_levels() {
        assert_eq!(
            resolve_permission("fully_auto"),
            everevo_sandbox::PermissionLevel::FullyAuto
        );
        assert_eq!(
            resolve_permission("fully_manual"),
            everevo_sandbox::PermissionLevel::FullyManual
        );
        assert_eq!(
            resolve_permission("read_only"),
            everevo_sandbox::PermissionLevel::ReadOnly
        );
    }

    #[test]
    fn test_resolve_permission_default_semiauto() {
        assert_eq!(
            resolve_permission(""),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
        assert_eq!(
            resolve_permission("unknown"),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
    }

    #[test]
    fn test_resolve_permission_case_sensitive() {
        assert_eq!(
            resolve_permission("FULLY_AUTO"),
            everevo_sandbox::PermissionLevel::SemiAuto
        );
    }
}
