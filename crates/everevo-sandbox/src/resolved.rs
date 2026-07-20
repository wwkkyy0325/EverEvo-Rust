//! Shell detection and resolution — WSL → Git Bash → PowerShell → CMD.
#![allow(clippy::disallowed_methods)] // Sandbox crate is the authorized Command::new caller

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellKind { Wsl = 0, GitBash = 1, PowerShell = 2, Cmd = 3, Unix = 10 }

#[derive(Debug, Clone)]
pub struct Shell {
    pub name: String,
    pub kind: ShellKind,
    pub executable: PathBuf,
    pub command_template: String,
}

pub struct ShellResolver;

impl ShellResolver {
    /// Cached result — shell detection is I/O heavy (process spawns).
    /// Once resolved, it's cached for the lifetime of the process.
    pub fn detect_all() -> Vec<Shell> {
        use std::sync::OnceLock;
        static CACHE: OnceLock<Vec<Shell>> = OnceLock::new();
        CACHE.get_or_init(Self::detect_all_uncached).clone()
    }

    fn detect_all_uncached() -> Vec<Shell> {
        let mut shells = Vec::new();
        #[cfg(windows)] { if let Some(s) = Self::find_wsl() { shells.push(s); } }
        #[cfg(windows)] { if let Some(s) = Self::find_git_bash() { shells.push(s); } }
        #[cfg(not(windows))] { shells.push(Shell { name: "sh".into(), kind: ShellKind::Unix, executable: PathBuf::from("/bin/sh"), command_template: "/bin/sh -c {command}".into() }); }
        #[cfg(windows)] {
            shells.push(Shell { name: "PowerShell".into(), kind: ShellKind::PowerShell, executable: PathBuf::from("powershell.exe"), command_template: "powershell.exe -NoProfile -Command {command}".into() });
            shells.push(Shell { name: "CMD".into(), kind: ShellKind::Cmd, executable: PathBuf::from("cmd.exe"), command_template: "cmd.exe /c {command}".into() });
        }
        shells
    }

    pub fn resolve() -> Option<Shell> { Self::detect_all().into_iter().next() }

    #[cfg(windows)]
    fn find_wsl() -> Option<Shell> {
        let wsl = which::which("wsl.exe").ok()?;
        // Step 1: check WSL is installed
        let version_out = std::process::Command::new(&wsl).args(["--version"]).output().ok()?;
        if !version_out.status.success() { return None; }
        // Step 2: health check — can it actually run a command?
        let test = std::process::Command::new(&wsl)
            .args(["-e", "sh", "-c", "echo wsl_ok"])
            .output()
            .ok()?;
        if test.status.success() && String::from_utf8_lossy(&test.stdout).contains("wsl_ok") {
            Some(Shell { name: "WSL".into(), kind: ShellKind::Wsl, executable: wsl, command_template: "wsl.exe -e sh -c {command}".into() })
        } else {
            tracing::warn!("WSL found but cannot execute commands — falling back to next shell");
            None
        }
    }

    #[cfg(windows)]
    fn find_git_bash() -> Option<Shell> {
        for path in &[r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"] {
            let p = PathBuf::from(path);
            if p.exists() { return Some(Shell { name: "Git Bash".into(), kind: ShellKind::GitBash, executable: p, command_template: format!("{} -c {{command}}", path) }); }
        }
        which::which("bash.exe").ok().map(|p| Shell { name: "Git Bash (PATH)".into(), kind: ShellKind::GitBash, executable: p, command_template: "bash.exe -c {command}".into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_detect_returns_shells() { assert!(!ShellResolver::detect_all().is_empty()); }
    #[test] fn test_resolve_returns_best() { assert!(ShellResolver::resolve().is_some()); }
}
