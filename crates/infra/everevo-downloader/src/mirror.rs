//! Multi-mirror URL resolver — domestic (CN) + international.
//!
//! ## How it works
//!
//! 1. The engine tries the original URL first.
//! 2. On failure, the mirror resolver extracts the path structure from the URL
//!    and tries candidate mirrors that are known to carry the same content.
//! 3. Mirrors are ranked by region priority and speed score.
//!
//! ## Pre-configured mirrors
//!
//! | Region | Mirror | Best for |
//! |--------|--------|-----------|
//! | CN | Tsinghua TUNA | Linux ISOs, language toolchains, package registries |
//! | CN | USTC | Similar to TUNA, good Anaconda/CTAN coverage |
//! | CN | Aliyun | Fast commercial CDN, good PyPI/NPM coverage |
//! | CN | Tencent Cloud | Good PyPI/NPM mirrors |
//! | CN | Huawei Cloud | Good Maven/PyPI mirrors |
//! | CN | NetEase (163) | Good Docker/PyPI mirrors |
//! | INT | jsDelivr | npm/GitHub CDN, works in CN too |
//! | INT | ghproxy | GitHub raw content proxy |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::task::Region;

// ── Mirror ──────────────────────────────────────────────────────────────

/// A mirror server with URL transformation rules.
#[derive(Clone)]
pub struct Mirror {
    pub name: String,
    pub base_url: String,
    pub region: Region,
    /// Speed score — higher = faster, updated dynamically (0..=100, default 50).
    pub speed_score: u8,
    /// Known host patterns this mirror can serve.
    /// e.g., `github.com` → mirror can rewrite GitHub URLs.
    pub serves_hosts: Vec<String>,
    /// URL transformation: source URL → mirror URL.
    pub transform: MirrorTransform,
}

/// How to transform a source URL into a mirror URL.
#[derive(Clone)]
pub enum MirrorTransform {
    /// Simple path append: `{base_url}/{path_and_query}`
    PathOnly,
    /// GitHub release: `github.com/{user}/{repo}/releases/download/{tag}/{file}`
    /// → `{base_url}/{user}/{repo}/{tag}/{file}`
    GitHubRelease,
    /// GitHub raw: `raw.githubusercontent.com/{user}/{repo}/{ref}/{path}`
    /// → `{base_url}/{user}/{repo}/{ref}/{path}`
    GitHubRaw,
    /// Custom function pointer (set programmatically).
    #[allow(clippy::type_complexity)]
    Custom(Arc<dyn Fn(&str) -> Option<String> + Send + Sync>),
}

impl Mirror {
    pub fn new_path_only(name: &str, base_url: &str, region: Region, serves: &[&str]) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            region,
            speed_score: 50,
            serves_hosts: serves.iter().map(|s| s.to_string()).collect(),
            transform: MirrorTransform::PathOnly,
        }
    }

    pub fn new_gh_release(name: &str, base_url: &str, region: Region) -> Self {
        let gh_hosts = &[
            "github.com",
            "objects.githubusercontent.com",
            "codeload.github.com",
        ];
        Self {
            name: name.into(),
            base_url: base_url.into(),
            region,
            speed_score: 50,
            serves_hosts: gh_hosts.iter().map(|s| s.to_string()).collect(),
            transform: MirrorTransform::GitHubRelease,
        }
    }

    pub fn new_gh_raw(name: &str, base_url: &str, region: Region) -> Self {
        let gh_hosts = &["raw.githubusercontent.com"];
        Self {
            name: name.into(),
            base_url: base_url.into(),
            region,
            speed_score: 50,
            serves_hosts: gh_hosts.iter().map(|s| s.to_string()).collect(),
            transform: MirrorTransform::GitHubRaw,
        }
    }

    /// Try to transform a source URL into this mirror's URL.
    pub fn try_map(&self, source_url: &str) -> Option<String> {
        // Check if this mirror serves this host
        let host = extract_host(source_url)?;
        if !self.serves_hosts.iter().any(|h| host.contains(h.as_str())) {
            return None;
        }

        match &self.transform {
            MirrorTransform::PathOnly => self.map_path_only(source_url),
            MirrorTransform::GitHubRelease => self.map_gh_release(source_url),
            MirrorTransform::GitHubRaw => self.map_gh_raw(source_url),
            MirrorTransform::Custom(f) => f(source_url),
        }
    }

    /// Extract path + query from URL and prepend to mirror base.
    fn map_path_only(&self, url: &str) -> Option<String> {
        let path_and_query = extract_path_and_query(url)?;
        let base = self.base_url.trim_end_matches('/');
        Some(format!("{base}/{path_and_query}"))
    }

    /// Transform: `github.com/{user}/{repo}/releases/download/{tag}/{file}`
    ///         → `{base_url}/{user}/{repo}/{tag}/{file}`
    fn map_gh_release(&self, url: &str) -> Option<String> {
        let path = extract_path(url)?;
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

        // Expected: [user, repo, "releases", "download", tag, file...]
        if segments.len() < 6 {
            return None;
        }
        let (user, repo, tag) = (segments[0], segments[1], segments[4]);
        let file: Vec<&str> = segments[5..].to_vec();

        let base = self.base_url.trim_end_matches('/');
        Some(format!("{base}/{user}/{repo}/{tag}/{}", file.join("/")))
    }

    /// Transform: `raw.githubusercontent.com/{user}/{repo}/{ref}/{path...}`
    ///         → `{base_url}/{user}/{repo}/{ref}/{path...}`
    fn map_gh_raw(&self, url: &str) -> Option<String> {
        let path = extract_path(url)?;
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

        // Expected: [user, repo, ref, path...]
        if segments.len() < 3 {
            return None;
        }
        let (user, repo, git_ref) = (segments[0], segments[1], segments[2]);
        let rest: Vec<&str> = segments[3..].to_vec();

        let base = self.base_url.trim_end_matches('/');
        if rest.is_empty() {
            Some(format!("{base}/{user}/{repo}/{git_ref}"))
        } else {
            Some(format!("{base}/{user}/{repo}/{git_ref}/{}", rest.join("/")))
        }
    }
}

// ── URL Parsing Helpers ─────────────────────────────────────────────────

fn extract_host(url: &str) -> Option<String> {
    // Simple string-based extraction (avoids heavy url crate in hot paths)
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = after_scheme.split('/').next()?;
    // Remove port if present
    let host = host.split(':').next()?;
    Some(host.to_lowercase())
}

fn extract_path(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let after_host = after_scheme.find('/')?;
    let path_and_query = &after_scheme[after_host..];
    // Strip query string
    let path = path_and_query.split('?').next()?;
    Some(path.to_string())
}

fn extract_path_and_query(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let after_host = after_scheme.find('/')?;
    Some(after_scheme[after_host + 1..].to_string())
}

// ── MirrorRegistry ──────────────────────────────────────────────────────

/// Registry of mirrors with region-aware resolution.
pub struct MirrorRegistry {
    mirrors: Vec<Mirror>,
    /// Map: source host → ordered list of mirror indices (scored by region+speed).
    host_map: Mutex<HashMap<String, Vec<usize>>>,
}

impl MirrorRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            mirrors: Vec::new(),
            host_map: Mutex::new(HashMap::new()),
        }
    }

    /// Create the default registry with all pre-configured mirrors.
    pub fn with_defaults() -> Self {
        let mut reg = Self::new();
        reg.register_domestic_defaults();
        reg.register_international_defaults();
        reg.rebuild_host_map();
        reg
    }

    /// Register a mirror.
    pub fn register(&mut self, mirror: Mirror) {
        self.mirrors.push(mirror);
    }

    /// Find candidate mirror URLs for a given source URL, ordered by priority.
    pub fn resolve(&self, url: &str, region_hint: Region) -> Vec<(String, String)> {
        let host = extract_host(url).unwrap_or_default();
        let map = self.host_map.lock().unwrap_or_else(|e| e.into_inner());

        let mut scored: Vec<(String, String)> = Vec::new();

        if let Some(indices) = map.get(&host) {
            for &idx in indices {
                if let Some(mirrored) = self.mirrors[idx].try_map(url) {
                    let score = self.score(&self.mirrors[idx], region_hint);
                    // Attach score to mirror name for sorting without re-parsing later
                    scored.push((
                        mirrored,
                        format!("{}__score__{score}", self.mirrors[idx].name),
                    ));
                }
            }
        }

        // Sort by score descending (higher = better), then by name for determinism
        scored.sort_by(|a, b| {
            let sa =
                a.1.rsplit("__score__")
                    .next()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);
            let sb =
                b.1.rsplit("__score__")
                    .next()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);
            sb.cmp(&sa).then_with(|| a.1.cmp(&b.1))
        });

        // Clean up score suffix
        for (_, name) in &mut scored {
            if let Some(pos) = name.rfind("__score__") {
                name.truncate(pos);
            }
        }

        scored
    }

    fn score(&self, mirror: &Mirror, hint: Region) -> i32 {
        let mut score = mirror.speed_score as i32;
        match hint {
            Region::Domestic if matches!(mirror.region, Region::Domestic) => score += 30,
            Region::International if matches!(mirror.region, Region::International) => score += 30,
            _ => {}
        }
        score
    }

    /// Rebuild the host-to-mirror index after registering new mirrors.
    pub fn rebuild_host_map(&mut self) {
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, mirror) in self.mirrors.iter().enumerate() {
            for host in &mirror.serves_hosts {
                map.entry(host.clone()).or_default().push(idx);
            }
        }
        *self.host_map.lock().unwrap_or_else(|e| e.into_inner()) = map;
    }

    /// Update speed score adaptively (Phase 2: EMA latency tracking).
    pub fn update_speed(&mut self, mirror_name: &str, success: bool, latency_ms: u64) {
        for m in &mut self.mirrors {
            if m.name == mirror_name {
                if success {
                    m.speed_score = (m.speed_score as u16 + 5).min(100) as u8;
                } else {
                    m.speed_score = (m.speed_score as u16).saturating_sub(10).max(10) as u8;
                }
                tracing::debug!(
                    mirror = mirror_name,
                    success,
                    latency_ms,
                    new_score = m.speed_score,
                    "Mirror speed updated"
                );
                return;
            }
        }
    }

    // ── Default Mirrors ──────────────────────────────────────────────

    fn register_domestic_defaults(&mut self) {
        let cn = Region::Domestic;

        // Tsinghua TUNA — GitHub releases
        self.register(Mirror::new_gh_release(
            "Tsinghua TUNA",
            "https://mirrors.tuna.tsinghua.edu.cn/github-release",
            cn,
        ));

        // USTC — general
        self.register(Mirror::new_path_only(
            "USTC",
            "https://mirrors.ustc.edu.cn",
            cn,
            &["github.com", "pypi.org", "repo1.maven.org", "nodejs.org"],
        ));

        // Aliyun
        self.register(Mirror::new_gh_release(
            "Aliyun GH Release",
            "https://mirrors.aliyun.com/github-release",
            cn,
        ));
        self.register(Mirror::new_path_only(
            "Aliyun PyPI",
            "https://mirrors.aliyun.com/pypi/simple",
            cn,
            &["pypi.org", "files.pythonhosted.org"],
        ));

        // Tencent Cloud
        self.register(Mirror::new_path_only(
            "Tencent Cloud",
            "https://mirrors.cloud.tencent.com",
            cn,
            &[
                "github.com",
                "pypi.org",
                "nodejs.org",
                "golang.org",
                "repo1.maven.org",
            ],
        ));

        // Huawei Cloud
        self.register(Mirror::new_path_only(
            "Huawei Cloud",
            "https://mirrors.huaweicloud.com",
            cn,
            &["github.com", "pypi.org", "repo1.maven.org", "nodejs.org"],
        ));

        // NetEase
        self.register(Mirror::new_path_only(
            "NetEase 163",
            "https://mirrors.163.com",
            cn,
            &[
                "github.com",
                "pypi.org",
                "dl-cdn.alpinelinux.org",
                "repo.mysql.com",
            ],
        ));

        // npmmirror CDN — general-purpose binary mirror for CN (primary)
        self.register(Mirror::new_path_only(
            "npmmirror CDN",
            "https://cdn.npmmirror.com/binaries",
            cn,
            &["github.com", "pypi.org", "nodejs.org", "repo1.maven.org"],
        ));

        // npmmirror registry (redirect-based mirror)
        self.register(Mirror::new_path_only(
            "npmmirror Registry",
            "https://registry.npmmirror.com/-/binary",
            cn,
            &["github.com", "pypi.org", "nodejs.org"],
        ));
    }

    fn register_international_defaults(&mut self) {
        let intl = Region::International;

        // jsDelivr — GitHub raw: user/repo@ref/path (global CDN)
        self.register(Mirror::new_gh_raw(
            "jsDelivr",
            "https://cdn.jsdelivr.net/gh",
            intl,
        ));

        // Fastly-backed jsDelivr
        self.register(Mirror::new_gh_raw(
            "Fastly GH",
            "https://fastly.jsdelivr.net/gh",
            intl,
        ));

        // ghproxy — simple URL prefix proxy
        let gh_all = &[
            "github.com",
            "raw.githubusercontent.com",
            "objects.githubusercontent.com",
        ];
        self.register(Mirror::new_path_only(
            "ghproxy",
            "https://ghproxy.com/https://github.com",
            intl,
            gh_all,
        ));
    }
}

impl Default for MirrorRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://github.com/user/repo/releases/download/v1/file.zip"),
            Some("github.com".into())
        );
        assert_eq!(
            extract_host("http://mirrors.tuna.tsinghua.edu.cn/path"),
            Some("mirrors.tuna.tsinghua.edu.cn".into())
        );
    }

    #[test]
    fn test_extract_path() {
        assert_eq!(
            extract_path("https://github.com/user/repo/releases/download/v1/file.zip"),
            Some("/user/repo/releases/download/v1/file.zip".into())
        );
    }

    #[test]
    fn test_gh_release_transform() {
        let mirror =
            Mirror::new_gh_release("Test", "https://mirror.example.com/releases", Region::Auto);
        let result =
            mirror.try_map("https://github.com/myuser/myrepo/releases/download/v1.0/file.zip");
        assert_eq!(
            result,
            Some("https://mirror.example.com/releases/myuser/myrepo/v1.0/file.zip".into())
        );
    }

    #[test]
    fn test_gh_raw_transform() {
        let mirror = Mirror::new_gh_raw("Test", "https://cdn.example.com/gh", Region::Auto);
        let result =
            mirror.try_map("https://raw.githubusercontent.com/myuser/myrepo/main/readme.md");
        assert_eq!(
            result,
            Some("https://cdn.example.com/gh/myuser/myrepo/main/readme.md".into())
        );
    }

    #[test]
    fn test_mirror_registry_resolve() {
        let reg = MirrorRegistry::with_defaults();
        let candidates = reg.resolve(
            "https://github.com/user/repo/releases/download/v1.0/tool.zip",
            Region::Domestic,
        );
        // Should find Tsinghua TUNA and Aliyun mirrors for GitHub releases
        assert!(
            !candidates.is_empty(),
            "Expected domestic mirror candidates"
        );
        for (url, name) in &candidates {
            println!("  {name}: {url}");
        }
    }
}
