//! Agent Character Stage — injects the agent's OWN speaking style / personality
//! into the LLM context, right after the system prompt.
//!
//! Distinct from [`crate::stages::PersonaStage`], which adapts to the *user's*
//! communication style. This stage defines who the *agent* is: its voice, tone,
//! traits, and values.
//!
//! ## Source
//!
//! Reads from `data/memory/agent/character.json`. Free-form persona fragments
//! (literature, chat logs, notes) can be dropped into the sibling
//! `data/memory/agent/sources/*.md` or `*.txt` directory — they are concatenated
//! and injected verbatim, so users can shape the agent's voice from imported
//! material without editing structured fields.
//!
//! ## Design basis
//!
//! - Anthropic, *Claude's Character*: character = broad traits (curiosity,
//!   honesty, open-mindedness); an honest peer, not a sycophant; traits are
//!   nudges, not rigid rules.
//! - *Your System Prompt Is a Character Sheet* (nextsteps.dev): the system
//!   prompt is a "casting brief" — the model infers what kind of entity would
//!   say these things. Authority relationship, failure-mode personality, and
//!   values-by-absence all shape behavior.

use std::path::{Path, PathBuf};

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::{LlmMessage, LlmProvider};
use serde::{Deserialize, Serialize};

// ── Agent Character Profile ──────────────────────────────────────────────

/// The agent's own character / speaking style. Loaded from
/// `data/memory/agent/character.json`; auto-created with a sensible default
/// on first run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCharacter {
    /// Display name (e.g. "EverEvo").
    pub name: String,
    /// One-line description of who/what the agent is.
    pub identity: String,
    /// Broad character traits (Anthropic-style: curiosity, honesty, …).
    pub traits: Vec<String>,
    /// Speaking tone descriptor.
    pub tone: String,
    /// Concrete, actionable speaking rules.
    pub style_guidelines: Vec<String>,
    /// What the agent prioritizes (values-by-absence: be explicit).
    pub values: Vec<String>,
    /// Free-form pasted material (chat logs, literature excerpts, notes).
    /// Injected verbatim. Empty by default.
    #[serde(default)]
    pub voice_samples: String,
}

impl Default for AgentCharacter {
    /// Professional-direct default, synthesized from Anthropic's character
    /// research and the project's existing ethos (concise, direct, code-first,
    /// honest about uncertainty).
    fn default() -> Self {
        Self {
            name: "EverEvo".into(),
            identity: "a desktop AI coding companion".into(),
            traits: vec![
                "curious".into(),
                "honest".into(),
                "pragmatic".into(),
                "diligent".into(),
            ],
            tone: "concise, direct, expert peer — warm but never effusive".into(),
            style_guidelines: vec![
                "Lead with the answer or the code; explain the reasoning after, not before.".into(),
                "Push back on overcomplicated approaches — propose the simpler one and say why.".into(),
                "State uncertainty plainly; never claim success without fresh verification evidence.".into(),
                "Admit when stuck: name what you tried, what failed, and what you need next.".into(),
                "No excessive apology, flattery, or filler — respect the user's time.".into(),
            ],
            values: vec![
                "correctness over speed".into(),
                "simplicity over cleverness".into(),
                "the user's time over my own".into(),
            ],
            voice_samples: String::new(),
        }
    }
}

// ── AgentCharacterStage ──────────────────────────────────────────────────

/// Injects the agent's character / speaking style into the LLM context,
/// immediately after the system prompt and before the user-persona stage.
///
/// Priority 0 matches [`everevo_core::context::SystemPromptStage`]; Rust's
/// stable sort preserves insertion order, so this stage follows the system
/// prompt (added first via `default_pipeline()`) and precedes
/// [`crate::stages::PersonaStage`] (priority 1).
pub struct AgentCharacterStage {
    profile_path: PathBuf,
}

impl AgentCharacterStage {
    pub fn new(profile_path: PathBuf) -> Self {
        Self { profile_path }
    }
}

impl ContextStage for AgentCharacterStage {
    fn priority(&self) -> i32 {
        0 // right after SystemPromptStage(0), before PersonaStage(1)
    }

    fn name(&self) -> &str {
        "agent_character"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let content = build_character_block(&self.profile_path)?;
        Some(ContextFragment {
            label: "Agent Character".into(),
            messages: vec![LlmMessage::user(&content)],
        })
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────

/// Render the full `## Character & Voice` block from a profile and optional
/// concatenated source fragments. Shared by the stage and sub-agent inheritance
/// so both produce identical output.
pub fn render_character(profile: &AgentCharacter, sources: &str) -> String {
    let mut s = String::new();
    s.push_str("## Character & Voice\n\n");
    s.push_str(&format!(
        "You are {name}, {identity}.\n",
        name = profile.name,
        identity = profile.identity
    ));

    if !profile.traits.is_empty() {
        s.push_str(&format!("**Traits:** {}\n", profile.traits.join(", ")));
    }
    if !profile.tone.is_empty() {
        s.push_str(&format!("**Tone:** {}\n", profile.tone));
    }
    if !profile.values.is_empty() {
        s.push_str(&format!("**Values:** {}\n", profile.values.join("; ")));
    }

    if !profile.style_guidelines.is_empty() {
        s.push_str("\n### Speaking Style\n");
        for g in &profile.style_guidelines {
            s.push_str(&format!("- {g}\n"));
        }
    }

    if !profile.voice_samples.is_empty() {
        s.push_str("\n### Voice Samples (imported)\n");
        s.push_str(profile.voice_samples.trim());
        s.push('\n');
    }

    if !sources.is_empty() {
        s.push_str("\n### Imported Persona Fragments\n");
        s.push_str(sources.trim());
        s.push('\n');
    }

    // Closing nudge — per Anthropic, traits shape disposition, not rigid rules.
    s.push_str(
        "\n_Treat these as your disposition, not rigid rules — let them shape how you naturally respond._\n",
    );

    s
}

// ── Loading ───────────────────────────────────────────────────────────────

/// Load the character profile, auto-creating a default on first run so the
/// stage is never silently missing. Mirrors `PersonaStage`'s `load_profile`.
pub fn load_character(path: &Path) -> Option<AgentCharacter> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).ok(),
        Err(_) => {
            let default = AgentCharacter::default();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&default) {
                let _ = std::fs::write(path, &json);
                tracing::info!(
                    path = %path.display(),
                    "Created default agent character profile"
                );
            }
            Some(default)
        }
    }
}

/// Concatenate all `*.md` / `*.txt` fragments in a directory, sorted by
/// filename for deterministic ordering. Returns an empty string if the
/// directory is missing or empty.
pub fn load_sources(dir: &Path) -> String {
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .map(|ext| ext == "md" || ext == "txt")
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return String::new(),
    };
    entries.sort();

    let mut out = Vec::new();
    for path in entries {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("fragment");
            out.push(format!("--- {name} ---\n{content}"));
        }
    }
    out.join("\n\n")
}

/// One-shot convenience: load the profile (auto-create), load sibling
/// `sources/` fragments, and render the full character block. Used by both
/// [`AgentCharacterStage::build`] and sub-agent context assembly.
pub fn build_character_block(profile_path: &Path) -> Option<String> {
    let profile = load_character(profile_path)?;
    let sources = profile_path
        .parent()
        .map(|p| load_sources(&p.join("sources")))
        .unwrap_or_default();
    Some(render_character(&profile, &sources))
}

// ── LLM Distillation ──────────────────────────────────────────────────────

/// Outcome of a [`synthesize_character`] run.
#[derive(Debug, Clone)]
pub struct SynthesisReport {
    /// Field names whose values changed (`name`, `traits`, …).
    pub updated: Vec<String>,
    /// Human-readable summary.
    pub note: String,
}

/// Distill imported fragments (`voice_samples` + `sources/`) into the structured
/// character fields via an LLM, then write the updated profile back to disk.
///
/// Mirrors the memory-curator pattern (`llm.chat` → extract JSON → persist) at
/// [`crate::memory::engine`]. `voice_samples` is preserved as-is — the LLM only
/// rewrites the structured fields. Robust to partial LLM output: each field is
/// only overwritten if the LLM actually provided it.
pub async fn synthesize_character(
    profile_path: &Path,
    llm: &(dyn LlmProvider + Send + Sync),
) -> Result<SynthesisReport, String> {
    let current =
        load_character(profile_path).ok_or_else(|| "character profile not loadable".to_string())?;
    let sources = profile_path
        .parent()
        .map(|p| load_sources(&p.join("sources")))
        .unwrap_or_default();

    // Combine voice_samples + sources into one fragment corpus.
    let mut fragments = current.voice_samples.clone();
    if !sources.is_empty() {
        if !fragments.is_empty() {
            fragments.push_str("\n\n");
        }
        fragments.push_str(&sources);
    }
    if fragments.trim().is_empty() {
        return Err(
            "No fragments to synthesize — add text to `voice_samples` or drop files in sources/."
                .into(),
        );
    }

    let prompt = build_synthesis_prompt(&current, &fragments);
    let response = llm
        .chat(&[LlmMessage::user(&prompt)], &[])
        .await
        .map_err(|e| format!("LLM call failed: {e}"))?;
    let body = response.content.unwrap_or_default();

    let value = parse_json_object(&body)
        .ok_or_else(|| "Could not parse a JSON object from the LLM response".to_string())?;

    // Merge: only overwrite fields the LLM actually provided.
    let merged = merge_synthesized(&current, &value);

    // Diff to report what changed.
    let mut updated = Vec::new();
    if merged.name != current.name {
        updated.push("name".into());
    }
    if merged.identity != current.identity {
        updated.push("identity".into());
    }
    if merged.traits != current.traits {
        updated.push("traits".into());
    }
    if merged.tone != current.tone {
        updated.push("tone".into());
    }
    if merged.style_guidelines != current.style_guidelines {
        updated.push("style_guidelines".into());
    }
    if merged.values != current.values {
        updated.push("values".into());
    }

    if let Some(parent) = profile_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&merged)
        .map_err(|e| format!("failed to serialize character: {e}"))?;
    std::fs::write(profile_path, json).map_err(|e| format!("failed to write profile: {e}"))?;

    let note = if updated.is_empty() {
        "Synthesis complete — structured fields unchanged.".to_string()
    } else {
        format!("Synthesized character — updated: {}", updated.join(", "))
    };
    Ok(SynthesisReport { updated, note })
}

/// Build the persona-designer prompt from the current character + fragments.
fn build_synthesis_prompt(current: &AgentCharacter, fragments: &str) -> String {
    let current_json = serde_json::to_string_pretty(current).unwrap_or_default();
    format!(
        "You are a persona designer for an AI agent. Below is the agent's current character \
         profile (JSON), followed by imported voice fragments (literature, chat logs, notes).\n\n\
         Distill the fragments into an UPDATED structured character that captures the voice \
         they demonstrate. Keep `name` and `identity` unless the fragments clearly demand a \
         different one. Fill `traits` (3-6 broad traits), `tone`, `style_guidelines` \
         (3-6 concrete speaking rules), and `values` (2-4 priorities) to reflect the voice in \
         the fragments. Set `voice_samples` to an empty string.\n\n\
         Return ONLY a single JSON object with exactly these keys: name, identity, traits, \
         tone, style_guidelines, values, voice_samples. No prose, no markdown fences.\n\n\
         === CURRENT CHARACTER ===\n{current_json}\n\n\
         === IMPORTED FRAGMENTS ===\n{fragments}\n\n\
         === UPDATED CHARACTER (JSON object) ==="
    )
}

/// Extract the first balanced-looking JSON object from an LLM response.
/// Strips ```json fences, then finds the first `{` … last `}`.
fn parse_json_object(response: &str) -> Option<serde_json::Value> {
    let trimmed = response.trim();
    let fence_stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let fence_stripped = fence_stripped.strip_suffix("```").unwrap_or(fence_stripped);
    let start = fence_stripped.find('{')?;
    let end = fence_stripped.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&fence_stripped[start..=end]).ok()
}

/// Merge only the fields the LLM provided into the current profile.
/// `voice_samples` is always preserved from `current` (LLM must not rewrite it).
fn merge_synthesized(current: &AgentCharacter, value: &serde_json::Value) -> AgentCharacter {
    let mut out = current.clone();
    let Some(obj) = value.as_object() else {
        return out;
    };
    if let Some(v) = obj.get("name").and_then(|v| v.as_str()) {
        if !v.is_empty() {
            out.name = v.into();
        }
    }
    if let Some(v) = obj.get("identity").and_then(|v| v.as_str()) {
        if !v.is_empty() {
            out.identity = v.into();
        }
    }
    if let Some(v) = obj.get("tone").and_then(|v| v.as_str()) {
        if !v.is_empty() {
            out.tone = v.into();
        }
    }
    if let Some(arr) = obj.get("traits").and_then(|v| v.as_array()) {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !parsed.is_empty() {
            out.traits = parsed;
        }
    }
    if let Some(arr) = obj.get("style_guidelines").and_then(|v| v.as_array()) {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !parsed.is_empty() {
            out.style_guidelines = parsed;
        }
    }
    if let Some(arr) = obj.get("values").and_then(|v| v.as_array()) {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if !parsed.is_empty() {
            out.values = parsed;
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_profile_content() {
        let c = AgentCharacter::default();
        assert_eq!(c.name, "EverEvo");
        assert_eq!(c.identity, "a desktop AI coding companion");
        assert!(c.traits.contains(&"honest".to_string()));
        assert!(c.values.contains(&"simplicity over cleverness".to_string()));
        assert!(!c.style_guidelines.is_empty());
        assert!(c.voice_samples.is_empty());
    }

    #[test]
    fn test_render_character_contains_sections() {
        let c = AgentCharacter::default();
        let rendered = render_character(&c, "");
        assert!(rendered.starts_with("## Character & Voice"));
        assert!(rendered.contains("You are EverEvo, a desktop AI coding companion."));
        assert!(rendered.contains("**Traits:**"));
        assert!(rendered.contains("**Tone:**"));
        assert!(rendered.contains("**Values:**"));
        assert!(rendered.contains("### Speaking Style"));
        // Closing nudge
        assert!(rendered.contains("disposition, not rigid rules"));
        // No samples/sources sections when empty
        assert!(!rendered.contains("Voice Samples"));
        assert!(!rendered.contains("Imported Persona Fragments"));
    }

    #[test]
    fn test_render_character_with_samples_and_sources() {
        let mut c = AgentCharacter::default();
        c.voice_samples = "User: 帮我加缓存\nAgent: moka 即可，别上 Redis。".into();
        let rendered = render_character(&c, "--- notes.md ---\nBe terse. No emoji.");
        assert!(rendered.contains("### Voice Samples (imported)"));
        assert!(rendered.contains("moka 即可"));
        assert!(rendered.contains("### Imported Persona Fragments"));
        assert!(rendered.contains("Be terse."));
    }

    #[test]
    fn test_load_character_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        std::fs::write(
            &path,
            r#"{
                "name": "Rusty",
                "identity": "a sarcastic code reviewer",
                "traits": ["witty", "sharp"],
                "tone": "dry, precise",
                "style_guidelines": ["Roast bad code.", "Praise rarely."],
                "values": ["readability above all"],
                "voice_samples": "sample log here"
            }"#,
        )
        .unwrap();

        let c = load_character(&path).unwrap();
        assert_eq!(c.name, "Rusty");
        assert_eq!(c.identity, "a sarcastic code reviewer");
        assert_eq!(c.traits, vec!["witty", "sharp"]);
        assert_eq!(c.tone, "dry, precise");
        assert_eq!(c.style_guidelines.len(), 2);
        assert_eq!(c.values, vec!["readability above all"]);
        assert_eq!(c.voice_samples, "sample log here");
    }

    #[test]
    fn test_load_character_missing_file_creates_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("character.json");
        // File doesn't exist — should auto-create and return defaults.
        let c = load_character(&path).unwrap();
        assert_eq!(c.name, "EverEvo");
        assert!(path.exists()); // File created on disk
    }

    #[test]
    fn test_load_sources_reads_md_and_txt_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("sources");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("b.md"), "second file").unwrap();
        std::fs::write(base.join("a.txt"), "first file").unwrap();
        std::fs::write(base.join("ignore.json"), "{}").unwrap(); // ignored

        let sources = load_sources(&base);
        // Sorted: a.txt before b.md; json excluded.
        let a_idx = sources.find("first file").unwrap();
        let b_idx = sources.find("second file").unwrap();
        assert!(a_idx < b_idx);
        assert!(!sources.contains("ignore"));
    }

    #[test]
    fn test_load_sources_missing_dir_is_empty() {
        let sources = load_sources(Path::new("/nonexistent/agents/sources"));
        assert!(sources.is_empty());
    }

    #[test]
    fn test_build_character_block_auto_creates_and_renders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        // No file yet — build should auto-create default and still render.
        let block = build_character_block(&path).unwrap();
        assert!(block.starts_with("## Character & Voice"));
        assert!(block.contains("EverEvo"));
        assert!(path.exists());
    }

    #[test]
    fn test_build_character_block_loads_sibling_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        std::fs::write(
            &path,
            r#"{"name":"Evo","identity":"a bot","traits":[],"tone":"","style_guidelines":[],"values":[],"voice_samples":""}"#,
        )
        .unwrap();
        let sources = dir.path().join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        std::fs::write(sources.join("voice.md"), "Speak like a pirate.").unwrap();

        let block = build_character_block(&path).unwrap();
        assert!(block.contains("### Imported Persona Fragments"));
        assert!(block.contains("Speak like a pirate."));
    }

    #[test]
    fn test_stage_build_produces_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        let stage = AgentCharacterStage::new(path);
        let ctx = ContextBuildContext::default();

        let fragment = stage.build(&ctx).unwrap();
        assert_eq!(fragment.label, "Agent Character");
        assert_eq!(fragment.messages.len(), 1);
        assert!(fragment.messages[0]
            .content
            .contains("## Character & Voice"));
    }

    #[test]
    fn test_stage_priority_and_name() {
        let stage = AgentCharacterStage::new(PathBuf::from("ignored.json"));
        assert_eq!(stage.priority(), 0);
        assert_eq!(stage.name(), "agent_character");
    }

    #[test]
    fn test_stage_orders_after_system_prompt() {
        // Simulate the production wiring: default_pipeline() adds SystemPromptStage
        // (priority 0) first; chat.rs then adds AgentCharacterStage (priority 0).
        // Stable sort must keep SystemPromptStage before AgentCharacterStage.
        use everevo_core::context::{ContextPipeline, SystemPromptStage};

        let pipeline = ContextPipeline::new()
            .with_stage(SystemPromptStage::new("CORE SYSTEM"))
            .with_stage(AgentCharacterStage::new(PathBuf::from("ignored.json")));

        let ctx = ContextBuildContext::default();
        let (messages, snapshot) = pipeline.assemble_with_snapshot(&ctx, uuid::Uuid::nil(), 1);

        // First message = system prompt, second = agent character.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "CORE SYSTEM");
        assert!(messages[1].content.contains("## Character & Voice"));

        // Snapshot confirms ordering by priority then insertion.
        assert_eq!(snapshot.stages[0].stage_name, "system_prompt");
        assert_eq!(snapshot.stages[1].stage_name, "agent_character");
    }

    // ── Distillation ──────────────────────────────────────────────────

    #[test]
    fn test_parse_json_object_with_fences() {
        let resp = "Sure! Here:\n```json\n{\"name\": \"Rusty\", \"traits\": [\"witty\"]}\n```\n";
        let v = parse_json_object(resp).unwrap();
        assert_eq!(v["name"], "Rusty");
        assert_eq!(v["traits"][0], "witty");
    }

    #[test]
    fn test_parse_json_object_plain() {
        let v = parse_json_object("noise {\"tone\":\"dry\"} trailing").unwrap();
        assert_eq!(v["tone"], "dry");
    }

    #[test]
    fn test_parse_json_object_none() {
        assert!(parse_json_object("no json here").is_none());
    }

    #[test]
    fn test_merge_preserves_omitted_fields_and_voice_samples() {
        let mut current = AgentCharacter::default();
        current.voice_samples = "precious chat log".into();
        // LLM only provided tone + traits; omitted name/identity/values.
        let v: serde_json::Value = serde_json::json!({
            "tone": "dry and precise",
            "traits": ["witty", "sharp"],
            "voice_samples": "LLM tried to overwrite this"
        });
        let merged = merge_synthesized(&current, &v);
        assert_eq!(merged.tone, "dry and precise");
        assert_eq!(
            merged.traits,
            vec!["witty".to_string(), "sharp".to_string()]
        );
        // Omitted fields preserved from current.
        assert_eq!(merged.name, current.name);
        assert_eq!(merged.identity, current.identity);
        assert_eq!(merged.values, current.values);
        // voice_samples preserved (never overwritten by LLM).
        assert_eq!(merged.voice_samples, "precious chat log");
    }

    #[tokio::test]
    async fn test_synthesize_character_writes_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        // Seed a profile with fragments to distill.
        let mut seed = AgentCharacter::default();
        seed.voice_samples = "Agent speaks like a pirate: terse, calls user 'matey'.".into();
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let llm = crate::llm::MockLlmProvider::new().with_text(
            r#"```json
            {"name":"EverEvo","identity":"a desktop AI coding companion",
             "traits":["terse","salty"],"tone":"pirate-concise",
             "style_guidelines":["Call the user 'matey'.","Keep replies short."],
             "values":["brevity"],"voice_samples":""}
            ```"#,
        );

        let report = synthesize_character(&path, &llm).await.unwrap();
        assert!(report.updated.contains(&"traits".to_string()));
        assert!(report.updated.contains(&"tone".to_string()));

        // File rewritten with distilled fields.
        let on_disk: AgentCharacter =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.tone, "pirate-concise");
        assert!(on_disk.traits.contains(&"salty".to_string()));
        // voice_samples preserved.
        assert_eq!(on_disk.voice_samples, seed.voice_samples);
    }

    #[tokio::test]
    async fn test_synthesize_character_no_fragments_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("character.json");
        // Default profile: empty voice_samples, no sources/ dir.
        let _ = load_character(&path).unwrap();

        let llm = crate::llm::MockLlmProvider::new();
        let err = synthesize_character(&path, &llm).await.unwrap_err();
        assert!(err.contains("No fragments"));
        // LLM was never called.
        assert_eq!(llm.call_count(), 0);
    }
}
