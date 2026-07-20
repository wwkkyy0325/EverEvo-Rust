//! Permission level and network policy types.

use serde::{Deserialize, Serialize};

// ── Permission Level ────────────────────────────────────────────────────

/// Permission level for an agent or a single operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionLevel {
    /// Read files, search, analyze. No writes, no shell, no network.
    ReadOnly = 0,
    /// Every command requires user confirmation before execution.
    FullyManual = 1,
    /// Safe commands auto-run. Dangerous commands + external paths → confirm.
    SemiAuto = 2,
    /// No confirmation required. Full audit trail. Admin commands still gated.
    FullyAuto = 3,
}

impl PermissionLevel {
    pub fn can_read(&self) -> bool {
        true
    }

    pub fn can_write(&self) -> bool {
        *self >= Self::FullyManual
    }

    pub fn can_shell(&self) -> bool {
        *self >= Self::FullyManual
    }

    pub fn can_network(&self) -> bool {
        *self >= Self::FullyManual
    }

    /// Does this level require user confirmation before execution?
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, Self::ReadOnly | Self::FullyManual | Self::SemiAuto)
    }

    /// Can this level auto-approve safe commands without user input?
    pub fn can_auto_approve(&self) -> bool {
        matches!(self, Self::SemiAuto | Self::FullyAuto)
    }

    /// Human-readable label.
    pub fn label(&self) -> &str {
        match self {
            Self::ReadOnly => "只读",
            Self::FullyManual => "纯手动",
            Self::SemiAuto => "半自动",
            Self::FullyAuto => "全自动",
        }
    }
}

// ── Network Policy ──────────────────────────────────────────────────────

/// Network access policy for sandbox execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkPolicy {
    Allowed,
    Denied,
    Restricted {
        allowed_hosts: Vec<String>,
        allowed_ports: Vec<u16>,
    },
}

impl NetworkPolicy {
    pub fn for_level(level: PermissionLevel) -> Self {
        match level {
            PermissionLevel::ReadOnly => Self::Denied,
            PermissionLevel::FullyManual => Self::Denied,
            PermissionLevel::SemiAuto => Self::Restricted {
                allowed_hosts: default_allowed_hosts(),
                allowed_ports: vec![80, 443, 8080, 3000],
            },
            PermissionLevel::FullyAuto => Self::Allowed,
        }
    }

    pub fn is_allowed(&self, host: &str, port: u16) -> bool {
        match self {
            Self::Allowed => true,
            Self::Denied => false,
            Self::Restricted {
                allowed_hosts,
                allowed_ports,
            } => {
                if !allowed_ports.contains(&port) {
                    return false;
                }
                allowed_hosts
                    .iter()
                    .any(|pattern| super::paths::glob_match(pattern, host))
            }
        }
    }
}

fn default_allowed_hosts() -> Vec<String> {
    vec![
        "pypi.org".into(),
        "*.python.org".into(),
        "registry.npmjs.org".into(),
        "*.npmmirror.com".into(),
        "crates.io".into(),
        "*.crates.io".into(),
        "hf-mirror.com".into(),
        "*.huggingface.co".into(),
        "github.com".into(),
        "*.github.com".into(),
        "api.deepseek.com".into(),
        "api.anthropic.com".into(),
        "localhost".into(),
        "127.0.0.0/8".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_ordering() {
        assert!(PermissionLevel::FullyAuto > PermissionLevel::SemiAuto);
        assert!(PermissionLevel::SemiAuto > PermissionLevel::FullyManual);
        assert!(PermissionLevel::FullyManual > PermissionLevel::ReadOnly);
    }

    #[test]
    fn test_readonly_no_write() {
        assert!(!PermissionLevel::ReadOnly.can_write());
        assert!(PermissionLevel::FullyManual.can_write());
    }
}
