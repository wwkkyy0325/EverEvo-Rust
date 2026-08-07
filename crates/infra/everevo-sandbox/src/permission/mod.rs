//! Permission model — 4 levels + path confinement + confirmation gating.
//!
//! ## Levels
//!
//! ```text
//! Level 0: ReadOnly    — read files, search, analyze. No writes, no shell.
//! Level 1: FullyManual — every command requires user confirmation.
//! Level 2: SemiAuto    — safe commands auto-run; dangerous commands +
//!                         out-of-sandbox paths → user decides.  (DEFAULT)
//! Level 3: FullyAuto   — no confirmation; full audit trail.
//!                         Admin commands (sudo/runas) STILL require confirmation.
//! ```
//!
//! ## Path Confinement (5-layer defense)
//!
//! 1. `work_dir` isolation — every command CWD = `data/sandbox/{id}/work/`
//! 2. Command string scan — extract absolute paths + shell redirect targets
//! 3. Allowlist / Denylist — glob-based path rules (deny wins over allow)
//! 4. Command pattern deny — destructive patterns blocked at any level
//! 5. Post-execution audit — stdout/stderr sizes logged for anomaly detection
//!
//! ## Design References
//!
//! - Claude Code 7-mode permission system (plan → bypassPermissions)
//! - IETF MAD Protocol: Narrowing Property (sub-agent ≤ delegator)
//! - AWS RAI 7-layer defense-in-depth governance

mod level;
mod paths;
mod patterns;
mod rules;

pub use level::{NetworkPolicy, PermissionLevel};
pub use rules::{check_permission, command_is_denied, PermissionDecision, PermissionRules};
