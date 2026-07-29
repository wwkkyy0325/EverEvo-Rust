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

use everevo_core::EverEvoError;

use crate::memory::frontmatter::parse_frontmatter;

// ── Built-in Skills ──────────────────────────────────────────────────────

/// Content for built-in skills, embedded in the binary at compile time.
/// Each entry: (directory_name, "SKILL.md content").
const BUILTIN_SKILLS: &[(&str, &str)] = &[(
    "anti-fixation",
    include_str!("../builtin-skills/anti-fixation/SKILL.md"),
)];

// ── Skill ─────────────────────────────────────────────────────────────────

/// A parsed skill from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub tools: Vec<String>,
    pub when_to_use: Vec<String>,
    pub persona: Option<String>,
    pub path: PathBuf,
}

// ── SkillRegistry ─────────────────────────────────────────────────────────

/// Scans `data/skills/` for SKILL.md files and builds an in-memory index.
pub struct SkillRegistry {
    pub(crate) skills: Vec<Skill>,
    pub(crate) skills_dir: PathBuf,
}

impl SkillRegistry {
    /// Create an empty registry — used as a last-resort fallback when
    /// all load attempts fail. Skills are non-critical; graceful degradation.
    pub fn empty() -> Self {
        Self {
            skills: Vec::new(),
            skills_dir: PathBuf::new(),
        }
    }

    /// Scan `skills_dir` for SKILL.md files, parse YAML frontmatter.
    pub fn load(skills_dir: &Path) -> Result<Self, EverEvoError> {
        let mut skills = Vec::new();

        if !skills_dir.exists() {
            return Ok(Self {
                skills,
                skills_dir: skills_dir.to_path_buf(),
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
            skills,
            skills_dir: skills_dir.to_path_buf(),
        })
    }

    /// Find skills relevant to a user message.
    ///
    /// Extracts significant words from each skill's `when_to_use` triggers
    /// and matches them against the user message using case-insensitive
    /// keyword overlap. Returns skills with at least one keyword match,
    /// sorted by match count.
    pub fn find_relevant(&self, user_message: &str) -> Vec<&Skill> {
        if user_message.trim().is_empty() {
            return vec![];
        }

        let msg_lower = user_message.to_lowercase();
        let mut scored: Vec<(usize, &Skill)> = self
            .skills
            .iter()
            .filter_map(|s| {
                let count = s
                    .when_to_use
                    .iter()
                    .filter(|trigger| trigger_keyword_match(trigger, &msg_lower))
                    .count();
                if count > 0 {
                    Some((count, s))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        scored.into_iter().map(|(_, s)| s).collect()
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// List all skill names + descriptions (for Stage 1 injection).
    pub fn list_metadata(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect()
    }

    /// Re-scan the skills directory and reload all SKILL.md files.
    /// Useful when skills are added/removed at runtime without restarting.
    pub fn rescan(&mut self) -> Result<(), everevo_core::EverEvoError> {
        let reloaded = SkillRegistry::load(&self.skills_dir)?;
        self.skills = reloaded.skills;
        Ok(())
    }

    /// Register built-in skills embedded in the binary at compile time.
    ///
    /// These are always available regardless of what's in `data/skills/`.
    /// Built-in skills are loaded BEFORE user skills; if a user skill has
    /// the same name, the user version takes precedence (last wins).
    pub fn with_builtins(mut self) -> Self {
        for (dir_name, content) in BUILTIN_SKILLS {
            // Use a synthetic path so parse_skill_md logs are meaningful.
            let fake_path = PathBuf::from("[builtin]").join(dir_name).join("SKILL.md");
            match parse_skill_md(content, &fake_path) {
                Ok(skill) => {
                    // Remove any existing skill with the same name (user override).
                    self.skills.retain(|s| s.name != skill.name);
                    tracing::info!(
                        name = %skill.name,
                        "Registered built-in skill"
                    );
                    self.skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!(
                        skill = %dir_name,
                        error = %e,
                        "Failed to parse built-in skill (skipping)"
                    );
                }
            }
        }
        self.skills.sort_by(|a, b| a.name.cmp(&b.name));
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
fn parse_skill_md(content: &str, path: &Path) -> Result<Skill, String> {
    let (fm, body) = parse_frontmatter(content).ok_or("No frontmatter found")?;

    let name = fm
        .get("name")
        .cloned()
        .ok_or_else(|| "Missing 'name' in frontmatter".to_string())?;
    let description = fm.get("description").cloned().unwrap_or_default();

    let tools = fm
        .get("tools")
        .map(|v| parse_list_value(v))
        .unwrap_or_default();

    let persona = fm.get("persona").cloned();

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
///
/// Extracts significant words (len > 2, not in stop list) from the trigger
/// and checks if any of them appear in the user message. A single keyword
/// match is sufficient.
fn trigger_keyword_match(trigger: &str, msg_lower: &str) -> bool {
    let trigger_lower = trigger.to_lowercase();
    trigger_lower
        .split_whitespace()
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(w))
        .any(|word| msg_lower.contains(word))
}

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
            },
            Skill {
                name: "diagram".into(),
                description: "Read diagrams".into(),
                body: "...".into(),
                tools: vec!["shell".into()],
                when_to_use: vec!["User provides an image".into()],
                persona: None,
                path: PathBuf::from("test"),
            },
        ];
        let registry = SkillRegistry {
            skills,
            skills_dir: PathBuf::from("test"),
        };

        let relevant = registry.find_relevant("Please do a code review on my PR");
        assert_eq!(relevant.len(), 1);
        assert_eq!(relevant[0].name, "code-review");

        let no_match = registry.find_relevant("Hello world");
        assert!(no_match.is_empty());

        let empty = registry.find_relevant("");
        assert!(empty.is_empty());
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
        }];
        let registry = SkillRegistry {
            skills,
            skills_dir: PathBuf::from("test"),
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
            },
            Skill {
                name: "b".into(),
                description: "desc b".into(),
                body: "".into(),
                tools: vec![],
                when_to_use: vec![],
                persona: None,
                path: PathBuf::from("test"),
            },
        ];
        let registry = SkillRegistry {
            skills,
            skills_dir: PathBuf::from("test"),
        };
        let meta = registry.list_metadata();
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].0, "a");
        assert_eq!(meta[1].1, "desc b");
    }

    #[test]
    fn test_with_builtins_registers_anti_fixation() {
        let registry = SkillRegistry::empty().with_builtins();
        let skill = registry.get("anti-fixation").expect("anti-fixation should be registered");
        assert!(skill.description.contains("fixation"));
        assert!(!skill.body.is_empty());
    }

    #[test]
    fn test_builtin_user_override_last_wins() {
        // User directory skill with same name as builtin — user wins
        let mut registry = SkillRegistry::empty();
        // Manually add a "user" skill with the same name
        registry.skills.push(Skill {
            name: "anti-fixation".into(),
            description: "user override version".into(),
            body: "custom".into(),
            tools: vec![],
            when_to_use: vec![],
            persona: None,
            path: PathBuf::from("data/skills/anti-fixation/SKILL.md"),
        });
        // with_builtins should remove user version and register builtin
        let registry = registry.with_builtins();
        let skill = registry.get("anti-fixation").unwrap();
        // Built-in wins (registered after user, removes duplicates first,
        // then inserts itself)
        assert!(skill.body.contains("Anti-Fixation Protocol"));
    }
}
