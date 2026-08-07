//! Slash command system — typed `/command` support for chat input.
//!
//! ## Architecture
//!
//! Commands are registered at startup (built-in + plugins). The chat route
//! checks incoming messages for a `/` prefix and dispatches to the handler
//! before the LLM context pipeline runs.
//!
//! ## Built-in commands
//!
//! | Command | Args | Description |
//! |---------|------|-------------|
//! | `/help` | — | List all available commands |
//! | `/clear` | — | Clear current session history |
//! | `/compact` | [topic] | Trigger context compaction |
//! | `/plan` | [task] | Enter plan mode; `/plan cancel` to exit |
//! | `/memory` | [query] | Search persistent memory |
//! | `/config` | — | Show current configuration |

use serde::Serialize;

/// A registered slash command definition.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommand {
    /// Command name without leading `/` (e.g. "help", "plan").
    pub name: String,
    /// One-line description shown in `/help` output and autocomplete.
    pub description: String,
    /// Optional argument placeholder (e.g. "query", "task").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_hint: Option<String>,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            args_hint: None,
        }
    }

    pub fn with_args(mut self, hint: impl Into<String>) -> Self {
        self.args_hint = Some(hint.into());
        self
    }

    /// Human-readable command line for display.
    pub fn display(&self) -> String {
        if let Some(ref hint) = self.args_hint {
            format!("/{} {}", self.name, hint)
        } else {
            format!("/{}", self.name)
        }
    }
}

/// Outcome of slash command dispatch.
pub enum CommandDispatch {
    /// Command was fully handled — an SSE event was sent, no LLM call needed.
    Handled,
    /// Command transforms the message and delegates to the LLM pipeline.
    /// The string replaces `req.message` for context assembly.
    Delegate(String),
}

/// Registry of slash commands available to the user.
#[derive(Debug, Clone, Default)]
pub struct SlashCommandRegistry {
    commands: Vec<SlashCommand>,
}

impl SlashCommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Register a command. Later registrations override earlier ones with the same name.
    pub fn register(&mut self, cmd: SlashCommand) {
        if let Some(existing) = self.commands.iter_mut().find(|c| c.name == cmd.name) {
            *existing = cmd;
        } else {
            self.commands.push(cmd);
        }
    }

    /// All registered commands (for `/help` output and API).
    pub fn list(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// Look up a command by name (without `/` prefix).
    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// Check if a message starts with a slash command.
    /// Returns `(command_name, remainder)` if found.
    pub fn parse<'a>(&self, message: &'a str) -> Option<(&'a str, &'a str)> {
        let trimmed = message.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }
        let rest = &trimmed[1..]; // strip '/'
                                  // Extract command name (up to first space or end)
        let (cmd_name, remainder) = match rest.find(' ') {
            Some(pos) => (&rest[..pos], rest[pos..].trim_start()),
            None => (rest, ""),
        };
        if cmd_name.is_empty() {
            return None;
        }
        // Only match if command is registered
        if self.commands.iter().any(|c| c.name == cmd_name) {
            Some((cmd_name, remainder))
        } else {
            None
        }
    }

    /// Format a `/help` response listing all commands.
    pub fn help_text(&self) -> String {
        let mut out = String::from("## Slash Commands\n\n");
        for cmd in &self.commands {
            let display = cmd.display();
            out.push_str(&format!("- `{display}` — {}\n", cmd.description));
        }
        out.push_str("\nType `/` in the chat input to see available commands.\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_parse_matches() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "Show help"));
        reg.register(SlashCommand::new("plan", "Plan mode"));

        assert_eq!(reg.parse("/help").map(|(n, r)| (n, r)), Some(("help", "")));
        assert_eq!(
            reg.parse("/plan my task").map(|(n, r)| (n, r)),
            Some(("plan", "my task"))
        );
    }

    #[test]
    fn test_registry_parse_unknown() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "Show help"));
        assert!(reg.parse("/unknown").is_none());
        assert!(reg.parse("hello").is_none());
    }

    #[test]
    fn test_registry_parse_empty() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "Show help"));
        assert!(reg.parse("/").is_none());
    }

    #[test]
    fn test_help_text() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand::new("help", "Show help"));
        reg.register(SlashCommand::new("clear", "Clear history"));
        let text = reg.help_text();
        assert!(text.contains("/help"));
        assert!(text.contains("/clear"));
    }

    #[test]
    fn test_command_display() {
        let cmd = SlashCommand::new("help", "Show help");
        assert_eq!(cmd.display(), "/help");

        let cmd = SlashCommand::new("plan", "Plan mode").with_args("task");
        assert_eq!(cmd.display(), "/plan task");
    }
}
