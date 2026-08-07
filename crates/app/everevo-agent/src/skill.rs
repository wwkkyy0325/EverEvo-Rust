//! Skill Registry — scans data/skills/ for user SKILL.md files and
//! merges with built-in skills embedded in the binary.
//!
//! ## Two sources, merged at startup
//!
//! 1. **Built-in** — shipped in the binary via `include_str!()`. Guaranteed
//!    present on every install, no filesystem dependency.
//! 2. **User** — loaded from `data/skills/` at runtime. Users can add their
//!    own skills without rebuilding.
//!
//! ## Two-stage injection pattern
//!
//! Stage 1 (SkillStage): inject only names + descriptions (~100 tokens each)
//! into the system context so the LLM knows what skills are available.
//!
//! Stage 2 (on-demand, future): when the LLM invokes a skill by name, the
//! full SKILL.md body is loaded and injected into the prompt.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use everevo_core::EverEvoError;
use serde::Serialize;

use crate::memory::frontmatter::parse_frontmatter;

// ── Built-in Skills ──────────────────────────────────────────────────────

/// Content for built-in skills, embedded in the binary at compile time.
/// Each entry: (directory_name, "SKILL.md content").
const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("anti-fixation", include_str!("../builtin-skills/anti-fixation/SKILL.md")),
    ("code-review", include_str!("../builtin-skills/code-review/SKILL.md")),
    ("debug-error", include_str!("../builtin-skills/debug-error/SKILL.md")),
    ("web-research", include_str!("../../everevo-webagent/builtin-skills/web-research/SKILL.md")),
    ("write-tests", include_str!("../builtin-skills/write-tests/SKILL.md")),
];

// ── Skill ─────────────────────────────────────────────────────────────────

/// A parsed skill from a SKILL.md file.
/// Where a skill was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SkillSource {
    Builtin,
    User,
    Project,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// Tool dependencies (legacy `tools:` or standard `allowed-tools:` frontmatter).
    pub tools: Vec<String>,
    /// Trigger phrases for auto-activation.
    pub when_to_use: Vec<String>,
    pub persona: Option<String>,
    pub path: PathBuf,
    /// Source location (builtin, user data/skills, project .everevo/skills).
    pub source: SkillSource,
    /// agentskills.io: if true, the model should NOT auto-invoke this skill.
    pub disable_model_invocation: bool,
    /// agentskills.io: override the model for this skill (sonnet/opus/haiku).
    pub model_override: Option<String>,
    /// agentskills.io: if false, only Claude can invoke (not the user).
    pub user_invocable: bool,
}

impl SkillSource {
    pub fn infer_from_path(path: &Path) -> Self {
        let s = path.to_string_lossy();
        if s.contains("[builtin]") {
            SkillSource::Builtin
        } else if s.contains(".everevo/skills") {
            SkillSource::Project
        } else {
            SkillSource::User
        }
    }
}

// ── SkillRegistry ─────────────────────────────────────────────────────────

/// Scans `data/skills/` for SKILL.md files and builds an in-memory index.
///
/// Uses `RwLock<Vec<Skill>>` so `check_rescan()` can hot-reload skills at
/// runtime without `&mut self` — new skills take effect immediately, no
/// restart required.
pub struct SkillRegistry {
    pub(crate) skills: RwLock<Vec<Skill>>,
    pub(crate) skills_dir: PathBuf,
    /// Last time `data/skills/` was scanned. Used by `check_rescan()` to
    /// detect new or modified SKILL.md files.
    pub(crate) last_scan: RwLock<SystemTime>,
}

impl SkillRegistry {
    /// Create an empty registry — used as a last-resort fallback when
    /// all load attempts fail. Skills are non-critical; graceful degradation.
    /// Path to the skills directory (for tools that need to write new skills).
    pub fn skills_dir(&self) -> PathBuf {
        self.skills_dir.clone()
    }

    pub fn empty() -> Self {
        Self {
            skills: RwLock::new(Vec::new()),
            skills_dir: PathBuf::new(),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
        }
    }

    /// Scan `skills_dir` for SKILL.md files, parse YAML frontmatter.
    pub fn load(skills_dir: &Path) -> Result<Self, EverEvoError> {
        let mut skills = Vec::new();

        if !skills_dir.exists() {
            return Ok(Self {
                skills: RwLock::new(skills),
                skills_dir: skills_dir.to_path_buf(),
                last_scan: RwLock::new(SystemTime::now()),
            });
        }

        let entries = fs::read_dir(skills_dir).map_err(|e| {
            EverEvoError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to read skills dir {}: {}", skills_dir.display(), e),
            ))
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            let content = match fs::read_to_string(&skill_md) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(path = %skill_md.display(), error = %e, "Failed to read SKILL.md");
                    continue;
                }
            };

            match parse_skill_md(&content, &skill_md) {
                Ok(skill) => {
                    tracing::info!(name = %skill.name, "Loaded skill");
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!(path = %skill_md.display(), error = %e, "Failed to parse SKILL.md");
                }
            }
        }

        // Sort by name for deterministic order
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self {
            skills: RwLock::new(skills),
            skills_dir: skills_dir.to_path_buf(),
            last_scan: RwLock::new(SystemTime::now()),
        })
    }

    /// Check whether `data/skills/` has changed since the last scan and, if so,
    /// hot-reload all SKILL.md files. Call this at the start of every request
    /// so newly promoted skills take effect immediately — no restart needed.
    pub fn check_rescan(&self) {
        // If the directory doesn't exist, there's nothing to scan.
        if !self.skills_dir.exists() {
            return;
        }
        // Quick path: walk skill dirs and check the newest SKILL.md file mtime.
        // Previously we only checked the directory mtime — that missed edits to
        // files inside existing skill directories (most filesystems only update
        // the parent dir's mtime on entry add/remove, not content changes).
        let last = *self.last_scan.read().unwrap_or_else(|e| e.into_inner());
        let mut newest = last;
        if let Ok(entries) = std::fs::read_dir(&self.skills_dir) {
            for entry in entries.flatten() {
                let skill_md = entry.path().join("SKILL.md");
                if let Ok(sm) = std::fs::metadata(&skill_md) {
                    if let Ok(m) = sm.modified() {
                        newest = newest.max(m);
                    }
                }
                // Also check dir mtime (covers new/removed skill dirs)
                if let Ok(dm) = entry.metadata() {
                    if let Ok(m) = dm.modified() {
                        newest = newest.max(m);
                    }
                }
            }
        }
        if newest <= last {
            return; // no changes
        }
        // Slow path: reload
        if let Ok(reloaded) = SkillRegistry::load(&self.skills_dir) {
            let count = reloaded.skills.read().unwrap_or_else(|e| e.into_inner()).len();
            let mut skills = self.skills.write().unwrap_or_else(|e| e.into_inner());
            let reloaded_skills = reloaded.skills.into_inner().unwrap_or_default();
            *skills = reloaded_skills;
            // Merge builtins again
            drop(skills);
            self.merge_builtins();
            let mut last = self.last_scan.write().unwrap_or_else(|e| e.into_inner());
            *last = SystemTime::now();
            tracing::info!(count, "Skill registry hot-reloaded");
        }
    }

    /// Merge built-in skills into the current skill list. Called on init and
    /// after each hot-reload so builtins are never lost.
    fn merge_builtins(&self) {
        let mut skills = self.skills.write().unwrap_or_else(|e| e.into_inner());
        for (dir_name, content) in BUILTIN_SKILLS {
            let fake_path = PathBuf::from("[builtin]").join(dir_name).join("SKILL.md");
            if let Ok(skill) = parse_skill_md(content, &fake_path) {
                skills.retain(|s| s.name != skill.name);
                skills.push(skill);
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Find skills relevant to a user message.
    ///
    /// Uses multi-signal scoring across `when_to_use` triggers, `description`,
    /// and `name` fields. Returns skills sorted by relevance score (descending).
    /// Skills with zero matches are filtered out.
    pub fn find_relevant(&self, user_message: &str) -> Vec<(Skill, f64)> {
        if user_message.trim().is_empty() {
            return vec![];
        }

        let guard = self.skills.read().unwrap_or_else(|e| e.into_inner());
        let msg = user_message.to_lowercase();
        let mut scored: Vec<(Skill, f64)> = guard
            .iter()
            .filter_map(|s| {
                let mut score: f64 = 0.0;

                // 1. Name exact match — weight 5.0
                if msg.contains(&s.name.to_lowercase()) {
                    score += 5.0;
                }

                // 2. when_to_use triggers — weight 3.0 per matching trigger
                for trigger in &s.when_to_use {
                    let keywords: Vec<&str> = trigger
                        .split_whitespace()
                        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(&w.to_lowercase().as_str()))
                        .collect();
                    if keywords.is_empty() {
                        continue;
                    }
                    let hits = keywords.iter().filter(|k| msg.contains(&k.to_lowercase())).count();
                    if hits > 0 {
                        score += 3.0 * (hits as f64 / keywords.len() as f64);
                    }
                }

                // 3. Description keyword overlap — weight 1.0
                let desc_words: Vec<&str> = s
                    .description
                    .split_whitespace()
                    .filter(|w| w.len() > 2 && !STOP_WORDS.contains(&w.to_lowercase().as_str()))
                    .collect();
                if !desc_words.is_empty() {
                    let desc_hits = desc_words
                        .iter()
                        .filter(|k| msg.contains(&k.to_lowercase()))
                        .count();
                    if desc_hits > 0 {
                        score += 1.0 * (desc_hits as f64 / desc_words.len() as f64);
                    }
                }

                if score > 0.0 {
                    Some((s.clone(), score))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // Cap at top 8 to avoid context bloat
        scored.truncate(8);
        scored
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<Skill> {
        let guard = self.skills.read().unwrap_or_else(|e| e.into_inner());
        guard.iter().find(|s| s.name == name).cloned()
    }

    /// List all skill names + descriptions (for Stage 1 injection).
    pub fn list_metadata(&self) -> Vec<(String, String)> {
        let guard = self.skills.read().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect()
    }

    /// Re-scan the skills directory and reload all SKILL.md files.
    /// Uses `&self` (interior mutability) — safe to call through `Arc`.
    pub fn rescan(&self) -> Result<(), everevo_core::EverEvoError> {
        let reloaded = SkillRegistry::load(&self.skills_dir)?;
        let reloaded_skills = reloaded.skills.into_inner().unwrap_or_default();
        let mut guard = self.skills.write().unwrap_or_else(|e| e.into_inner());
        *guard = reloaded_skills;
        drop(guard);
        self.merge_builtins();
        Ok(())
    }

    /// Register built-in skills embedded in the binary at compile time.
    /// Delegates to `merge_builtins()` — same logic for init and hot-reload.
    pub fn with_builtins(self) -> Self {
        self.merge_builtins();
        self
    }
}

// ── Frontmatter Parsing ───────────────────────────────────────────────────

/// Parse a list value from YAML frontmatter.
///
/// Supports two formats:
/// 1. Inline bracket: `[item1, item2]`
/// 2. YAML list (bullet): indented `- item1\n  - item2`
fn parse_list_value(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    // Format 1: [item1, item2]
    if raw.starts_with('[') && raw.ends_with(']') {
        let inner = &raw[1..raw.len() - 1];
        return inner
            .split(',')
            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    // Format 2: single value without brackets
    if !raw.is_empty() {
        return vec![raw.to_string()];
    }
    vec![]
}

/// Parse a SKILL.md file into a `Skill`.
/// Write a `SKILL.md` to the skills library, promoting a repeatable procedure
/// into a discoverable skill (auto-surfaced via `when_to_use` triggers once the
/// registry reloads — next session start or a future `rescan`).
pub fn promote_to_skill(
    skills_dir: &std::path::Path,
    name: &str,
    description: &str,
    when_to_use: &[String],
    body: &str,
) -> Result<std::path::PathBuf, everevo_core::EverEvoError> {
    let safe: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if safe.is_empty() {
        return Err(everevo_core::EverEvoError::InvalidInput(format!(
            "invalid skill name: {name}"
        )));
    }
    let dir = skills_dir.join(&safe);
    std::fs::create_dir_all(&dir)
        .map_err(|e| everevo_core::EverEvoError::Internal(format!("create skill dir: {e}")))?;
    let triggers = when_to_use
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let content = format!(
        "---\nname: {safe}\ndescription: {description}\nwhen_to_use:\n{triggers}\n---\n\n{body}\n"
    );
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content)
        .map_err(|e| everevo_core::EverEvoError::Internal(format!("write SKILL.md: {e}")))?;
    tracing::info!(name = %safe, path = %path.display(), "Skill promoted to library");
    Ok(path)
}

/// Tool for the LLM to promote a repeatable procedure into a discoverable skill.
pub struct PromoteSkillTool {
    skills_dir: std::path::PathBuf,
}

impl PromoteSkillTool {
    pub fn new(skills_dir: std::path::PathBuf) -> Self {
        Self { skills_dir }
    }
}

#[async_trait::async_trait]
impl everevo_core::tool::Tool for PromoteSkillTool {
    fn name(&self) -> &str {
        "promote_to_skill"
    }
    fn description(&self) -> &str {
        "Promote a repeatable procedure into a discoverable skill (writes \
         data/skills/<name>/SKILL.md). Skills auto-surface in future sessions via \
         their `when_to_use` triggers. Use for procedures you keep repeating and \
         want the agent to recall automatically. Parameters: name, description, \
         when_to_use (trigger phrases), body (the skill instructions)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "when_to_use": {"type": "array", "items": {"type": "string"}, "description": "trigger phrases"},
                "body": {"type": "string", "description": "the skill instructions (markdown)"}
            },
            "required": ["name", "description", "when_to_use", "body"]
        })
    }
    fn risk_level(&self) -> everevo_core::types::RiskLevel {
        everevo_core::types::RiskLevel::Low
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<everevo_core::tool::ToolOutput, everevo_core::EverEvoError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| everevo_core::EverEvoError::InvalidInput("name is required".into()))?;
        let description = params["description"].as_str().unwrap_or("");
        let body = params["body"].as_str().unwrap_or("");
        let when_to_use: Vec<String> = params["when_to_use"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let path = promote_to_skill(&self.skills_dir, name, description, &when_to_use, body)?;
        Ok(everevo_core::tool::ToolOutput {
            content: format!(
                "Promoted skill '{}' → {}. It will auto-surface via its triggers in future sessions (registry reloads on restart).",
                name,
                path.display()
            ),
            is_error: false,
            ..Default::default()
        })
    }
}

fn parse_skill_md(content: &str, path: &Path) -> Result<Skill, String> {
    let (fm, body) = parse_frontmatter(content).ok_or("No frontmatter found")?;

    let name = fm
        .get("name")
        .cloned()
        .ok_or_else(|| "Missing 'name' in frontmatter".to_string())?;
    let description = fm.get("description").cloned().unwrap_or_default();

    // tools: try "allowed-tools" (agentskills.io standard) first, fall back to "tools" (legacy)
    let tools_raw = fm
        .get("allowed-tools")
        .or_else(|| fm.get("tools"))
        .cloned();
    let tools = tools_raw
        .as_deref()
        .map(parse_list_value)
        .unwrap_or_default();

    let persona = fm.get("persona").cloned();
    let disable_model_invocation = fm
        .get("disable-model-invocation")
        .map(|v| matches!(v.as_str(), "true" | "yes" | "1"))
        .unwrap_or(false);
    let model_override = fm.get("model").cloned();
    let user_invocable = fm
        .get("user-invocable")
        .map(|v| !matches!(v.as_str(), "false" | "no" | "0"))
        .unwrap_or(true);

    let when_to_use = if let Some(raw) = fm.get("when_to_use") {
        if raw.is_empty() {
            parse_multiline_when_to_use(content)
        } else {
            parse_list_value(raw)
        }
    } else {
        vec![]
    };

    Ok(Skill {
        name,
        description,
        body: body.to_string(),
        tools,
        when_to_use,
        persona,
        path: path.to_path_buf(),
        source: SkillSource::infer_from_path(path),
        disable_model_invocation,
        model_override,
        user_invocable,
    })
}

/// Parse when_to_use from raw content when it spans multiple lines.
fn parse_multiline_when_to_use(content: &str) -> Vec<String> {
    let mut in_when_to_use = false;
    let mut items = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "when_to_use:" {
            in_when_to_use = true;
            continue;
        }
        if in_when_to_use {
            if let Some(stripped) = trimmed.strip_prefix("- ") {
                items.push(stripped.trim().to_string());
            } else if !trimmed.starts_with('-')
                && !trimmed.is_empty()
                && !trimmed.starts_with("---")
            {
                if trimmed.contains(':') {
                    break;
                }
            } else if trimmed.starts_with("---") {
                break;
            }
        }
    }
    items
}

// ── Keyword Match Helper ──────────────────────────────────────────────────

const STOP_WORDS: &[&str] = &[
    "user", "asks", "ask", "for", "an", "or", "to", "when", "the", "a", "wants", "want",
    "provides", "provide", "has", "have", "is", "are", "be", "in", "on", "at", "with", "about",
    "check", "needs", "need",
];

/// Check if a skill trigger matches a user message via keyword overlap.
// (trigger_keyword_match removed — keyword extraction now inline in find_relevant)

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Frontmatter parsing ───────────────────────────────────────────

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n\nBody text";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("name").unwrap(), "test-skill");
        assert_eq!(fm.get("description").unwrap(), "A test skill");
        assert_eq!(body, "Body text");
    }

    #[test]
    fn test_parse_list_inline() {
        let items = parse_list_value("[shell, memory]");
        assert_eq!(items, vec!["shell", "memory"]);
    }

    #[test]
    fn test_parse_list_empty() {
        let items = parse_list_value("[]");
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_multiline_when_to_use() {
        let content = "---\nname: test\nwhen_to_use:\n  - User provides image\n  - User asks for diagram\n---\n\nBody";
        let items = parse_multiline_when_to_use(content);
        assert_eq!(items, vec!["User provides image", "User asks for diagram"]);
    }

    // ── Skill parsing ─────────────────────────────────────────────────

    #[test]
    fn test_parse_skill_md() {
        let content = "\
---
name: code-review
description: Review code for bugs, style issues, and security problems
tools: [shell, memory]
when_to_use:
  - User asks for code review
  - User wants to check code quality
---
# Code Review Skill

Some body text.";
        let skill = parse_skill_md(content, Path::new("data/skills/code-review/SKILL.md")).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(
            skill.description,
            "Review code for bugs, style issues, and security problems"
        );
        assert_eq!(skill.tools, vec!["shell", "memory"]);
        assert_eq!(
            skill.when_to_use,
            vec![
                "User asks for code review",
                "User wants to check code quality",
            ]
        );
        assert!(skill.body.contains("Code Review Skill"));
        assert!(skill.persona.is_none());
    }

    #[test]
    fn test_parse_skill_with_persona() {
        let content = "\
---
name: read-diagram
description: Extract structured info from images/diagrams
tools: [shell]
when_to_use:
  - User provides an image or diagram
persona: You are a senior architect.
---
Body here.";
        let skill = parse_skill_md(content, Path::new("test/SKILL.md")).unwrap();
        assert_eq!(skill.name, "read-diagram");
        assert_eq!(
            skill.persona.as_deref(),
            Some("You are a senior architect.")
        );
    }

    // ── SkillRegistry ─────────────────────────────────────────────────

    #[test]
    fn test_registry_find_relevant() {
        let skills = vec![
            Skill {
                name: "code-review".into(),
                description: "Review code".into(),
                body: "...".into(),
                tools: vec!["shell".into()],
                when_to_use: vec!["User asks for code review".into()],
                persona: None,
                path: PathBuf::from("test"),
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
            },
            Skill {
                name: "diagram".into(),
                description: "Read diagrams".into(),
                body: "...".into(),
                tools: vec!["shell".into()],
                when_to_use: vec!["User provides an image".into()],
                persona: None,
                path: PathBuf::from("test"),
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
            },
        ];
        let registry = SkillRegistry {
            skills: RwLock::new(skills),
            skills_dir: PathBuf::from("test"),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
        };

        let relevant = registry.find_relevant("Please do a code review on my PR");
        assert_eq!(relevant.len(), 1);
        assert_eq!(relevant[0].0.name, "code-review");

        let no_match = registry.find_relevant("Hello world");
        assert!(no_match.is_empty());

        let empty = registry.find_relevant("");
        assert!(empty.is_empty());
    }

    #[test]
    fn test_promote_to_skill_writes_valid_skill_md() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = promote_to_skill(
            dir.path(),
            "deploy-app",
            "Deploys the app",
            &["user asks to deploy".into(), "release the app".into()],
            "Run: npm run deploy",
        )
        .unwrap();
        assert!(path.ends_with("deploy-app/SKILL.md"));
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("name: deploy-app"));
        assert!(written.contains("user asks to deploy"));
        assert!(written.contains("Run: npm run deploy"));
        // Round-trip: parse_skill_md can read it back.
        let skill = parse_skill_md(&written, &path).unwrap();
        assert_eq!(skill.name, "deploy-app");
        assert_eq!(skill.when_to_use.len(), 2);
    }

    #[test]
    fn test_promote_to_skill_rejects_bad_name() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(promote_to_skill(dir.path(), "!!!", "x", &[], "x").is_err());
    }

    #[test]
    fn test_registry_get_by_name() {
        let skills = vec![Skill {
            name: "code-review".into(),
            description: "desc".into(),
            body: "body".into(),
            tools: vec![],
            when_to_use: vec![],
            persona: None,
            path: PathBuf::from("test"),
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
        }];
        let registry = SkillRegistry {
            skills: RwLock::new(skills),
            skills_dir: PathBuf::from("test"),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
        };

        assert!(registry.get("code-review").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_list_metadata() {
        let skills = vec![
            Skill {
                name: "a".into(),
                description: "desc a".into(),
                body: "".into(),
                tools: vec![],
                when_to_use: vec![],
                persona: None,
                path: PathBuf::from("test"),
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
            },
            Skill {
                name: "b".into(),
                description: "desc b".into(),
                body: "".into(),
                tools: vec![],
                when_to_use: vec![],
                persona: None,
                path: PathBuf::from("test"),
            source: crate::skill::SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
            },
        ];
        let registry = SkillRegistry {
            skills: RwLock::new(skills),
            skills_dir: PathBuf::from("test"),
            last_scan: RwLock::new(SystemTime::UNIX_EPOCH),
        };
        let meta = registry.list_metadata();
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].0, "a");
        assert_eq!(meta[1].1, "desc b");
    }

    #[test]
    fn test_with_builtins_registers_anti_fixation() {
        let registry = SkillRegistry::empty().with_builtins();
        let skill = registry
            .get("anti-fixation")
            .expect("anti-fixation should be registered");
        assert!(skill.description.contains("fixation"));
        assert!(!skill.body.is_empty());
    }

    #[test]
    fn test_builtin_user_override_last_wins() {
        // User directory skill with same name as builtin — user wins
        let registry = SkillRegistry::empty();
        // Manually add a "user" skill with the same name
        registry.skills.write().unwrap().push(Skill {
            name: "anti-fixation".into(),
            description: "user override version".into(),
            body: "custom".into(),
            tools: vec![],
            when_to_use: vec![],
            persona: None,
            path: PathBuf::from("data/skills/anti-fixation/SKILL.md"),
            source: SkillSource::User,
            disable_model_invocation: false,
            model_override: None,
            user_invocable: true,
        });
        // with_builtins should remove user version and register builtin
        let registry = registry.with_builtins();
        let skill = registry.get("anti-fixation").unwrap();
        // Built-in wins (registered after user, removes duplicates first,
        // then inserts itself)
        assert!(skill.body.contains("Anti-Fixation Protocol"));
    }
}
