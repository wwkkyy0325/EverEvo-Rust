//! Skill Registry — scans data/skills/ for SKILL.md files, parses YAML
//! frontmatter, and builds an index for context injection.
//!
//! ## Two-stage injection pattern
//!
//! Stage 1 (SkillStage): inject only names + descriptions (~100 tokens each)
//! into the system context so the LLM knows what skills are available.
//!
//! Stage 2 (on-demand, future): when the LLM invokes a skill by name, the
//! full SKILL.md body is loaded and injected into the prompt.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;
use everevo_core::EverEvoError;

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
    skills: Vec<Skill>,
    #[allow(dead_code)]
    skills_dir: PathBuf,
}

impl SkillRegistry {
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

        scored.sort_by(|a, b| b.0.cmp(&a.0));
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
}

// ── SkillStage (ContextPipeline Integration) ──────────────────────────────

/// Injects available skill names + descriptions into the LLM context.
///
/// Stage 1 only — lightweight metadata injection so the LLM knows what
/// skills exist. Full skill bodies are loaded on-demand in Stage 2 (future).
pub struct SkillStage {
    registry: Arc<SkillRegistry>,
}

impl SkillStage {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

impl ContextStage for SkillStage {
    fn priority(&self) -> i32 {
        2 // after PersonaStage(1), before MemoryStage(3)
    }

    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let metadata = self.registry.list_metadata();
        if metadata.is_empty() {
            return None;
        }
        let content: String = metadata
            .iter()
            .map(|(name, desc)| format!("- **{name}**: {desc}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(ContextFragment {
            label: "Available Skills".into(),
            messages: vec![LlmMessage::user(&format!(
                "## Available Skills\n\n{content}\n\n\
                 To use a skill, say \"use the {skill_name} skill\" or invoke it by name.",
                skill_name = "{name}"
            ))],
        })
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
    let description = fm
        .get("description")
        .cloned()
        .unwrap_or_default();

    let tools = fm
        .get("tools")
        .map(|v| parse_list_value(v))
        .unwrap_or_default();

    let persona = fm.get("persona").cloned();

    // when_to_use may be an inline list or a multiline YAML list.
    // If the frontmatter key is empty (multi-line), we look at the raw
    // frontmatter lines to extract the list.
    let when_to_use = if let Some(raw) = fm.get("when_to_use") {
        if raw.is_empty() {
            // Multiline — we need to re-parse from the raw frontmatter text
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
            } else if !trimmed.starts_with('-') && !trimmed.is_empty() && !trimmed.starts_with("---") {
                // End of the list — not indented and not a dash item
                if trimmed.contains(':') {
                    // Next frontmatter key
                    break;
                }
            } else if trimmed.starts_with("---") {
                break;
            }
        }
    }
    items
}

/// Parse YAML-like frontmatter from markdown content.
/// Returns `(key_value_map, body_text)` or `None` if no frontmatter found.
fn parse_frontmatter(content: &str) -> Option<(HashMap<String, String>, &str)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("\n---")?;
    let fm_text = &rest[..end];
    let body = rest[end + 4..].trim();

    let mut map = HashMap::new();
    let mut pending_key: Option<String> = None;

    for line in fm_text.lines() {
        if let Some(pending) = pending_key.take() {
            // This line is a value for the pending key
            let trimmed = line.trim();
            if trimmed.starts_with("- ") {
                // Multiline list continues — handled elsewhere by the caller,
                // but we need to ingest at least the first item here.
                // For now, store an empty marker so the caller knows to re-parse.
                map.insert(pending, String::new());
            } else if !trimmed.is_empty() {
                map.insert(pending, trimmed.to_string());
            }
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if value.is_empty() {
                // Could be a multiline value — defer to next line
                pending_key = Some(key);
            } else {
                map.insert(key, value);
            }
        }
    }

    Some((map, body))
}

// ── Keyword Match Helper ──────────────────────────────────────────────────

/// Common stop words that don't carry semantic meaning for trigger matching.
const STOP_WORDS: &[&str] = &[
    "user", "asks", "ask", "for", "an", "or", "to", "when", "the", "a",
    "wants", "want", "provides", "provide", "has", "have", "is", "are",
    "be", "in", "on", "at", "with", "about", "check", "needs", "need",
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
        assert_eq!(skill.description, "Review code for bugs, style issues, and security problems");
        assert_eq!(skill.tools, vec!["shell", "memory"]);
        assert_eq!(skill.when_to_use, vec![
            "User asks for code review",
            "User wants to check code quality",
        ]);
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
        assert_eq!(skill.persona.as_deref(), Some("You are a senior architect."));
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

    // ── SkillStage ────────────────────────────────────────────────────

    #[test]
    fn test_skill_stage_builds_context_fragment() {
        let skills = vec![Skill {
            name: "test-skill".into(),
            description: "A test skill for testing".into(),
            body: "body".into(),
            tools: vec![],
            when_to_use: vec![],
            persona: None,
            path: PathBuf::from("test"),
        }];
        let registry = Arc::new(SkillRegistry {
            skills,
            skills_dir: PathBuf::from("test"),
        });
        let stage = SkillStage::new(registry);
        let ctx = ContextBuildContext::default();

        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.label, "Available Skills");
        assert_eq!(fragment.messages.len(), 1);
        let content = &fragment.messages[0].content;
        assert!(content.contains("Available Skills"));
        assert!(content.contains("**test-skill**"));
        assert!(content.contains("A test skill for testing"));
    }

    #[test]
    fn test_skill_stage_empty_registry_returns_none() {
        let registry = Arc::new(SkillRegistry {
            skills: vec![],
            skills_dir: PathBuf::from("test"),
        });
        let stage = SkillStage::new(registry);
        let ctx = ContextBuildContext::default();
        assert!(stage.build(&ctx).is_none());
    }
}
