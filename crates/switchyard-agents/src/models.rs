//! Model choices per provider, read from the installed native CLI (M8.1).
//!
//! Switchyard keeps no model catalogue of its own. Each adapter reports what
//! its installed CLI exposes:
//!
//! * **Claude Code** has no command that lists models. Its `--help` names the
//!   aliases `--model` accepts (2.1.325: `fable`, `opus`, `sonnet`) and says it
//!   also takes "a model's full name". Switchyard offers exactly the aliases the
//!   installed CLI's help names, and accepts a full name (`claude-…`) entered by
//!   the operator. Whether a full name exists can only be known when the CLI
//!   starts; Switchyard never passes `--fallback-model`, so an unknown model
//!   fails the session instead of being replaced.
//! * **Codex** answers `model/list` on its app-server (0.166.0). Switchyard
//!   offers and accepts exactly the models it lists.
//!
//! No model is the same as "the CLI's default": shown explicitly, never hidden.

use serde::{Deserialize, Serialize};

use crate::model::AgentProvider;

/// Longest model identifier accepted.
pub const MAX_MODEL_ID: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    /// Exactly what is passed to the CLI.
    pub id: String,
    pub display_name: String,
    /// The CLI's own default (only when the CLI says so).
    pub is_default: bool,
}

/// What the installed CLI of one provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub provider: AgentProvider,
    pub models: Vec<ModelOption>,
    /// Whether a model's full name (not listed) may be entered.
    pub accepts_full_names: bool,
    /// Where the list comes from, for the UI.
    pub source: String,
    /// Set when the list could not be read; choosing a model is then refused
    /// (the CLI default still works).
    pub error: Option<String>,
}

impl ModelCatalog {
    /// Checks a model choice. `None` (the CLI default) is always allowed.
    pub fn validate(&self, model: Option<&str>) -> Result<(), String> {
        let Some(model) = model else { return Ok(()) };
        let name = crate::display_name(self.provider);
        if model.is_empty()
            || model.len() > MAX_MODEL_ID
            || model.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!("{model:?} is not a model identifier"));
        }
        if self.models.iter().any(|m| m.id == model) {
            return Ok(());
        }
        if self.accepts_full_names && is_claude_full_name(model) {
            return Ok(());
        }
        if let Some(e) = &self.error {
            return Err(format!("cannot check model {model:?} for {name}: {e}"));
        }
        let known = self.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ");
        let full = if self.accepts_full_names { " or a full model name (claude-…)" } else { "" };
        Err(format!("{name} does not offer model {model:?}; choose one of: {known}{full} ({})", self.source))
    }
}

/// A Claude model's full name, e.g. `claude-sonnet-4-5` (lowercase letters,
/// digits, dots and dashes).
fn is_claude_full_name(model: &str) -> bool {
    model.strip_prefix("claude-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    })
}

/// The aliases Claude's `--help` names for `--model`: the quoted words in the
/// option's description, e.g. `(e.g. 'fable', 'opus', or 'sonnet')`.
pub fn claude_aliases_from_help(help: &str) -> Vec<String> {
    let Some(start) = help.find("--model") else { return Vec::new() };
    // The description runs until the next option.
    let rest = &help[start + "--model".len()..];
    let end = rest.find("\n  -").unwrap_or(rest.len());
    let description = &rest[..end];
    let mut aliases = Vec::new();
    let mut parts = description.split('\'');
    parts.next();
    while let (Some(word), Some(_)) = (parts.next(), parts.next()) {
        let ok = !word.is_empty()
            && word.len() <= 32
            && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if ok && !aliases.iter().any(|a| a == word) {
            aliases.push(word.to_owned());
        }
    }
    aliases
}

/// The catalogue for Claude Code from the aliases its installed CLI names.
pub fn claude_catalog(aliases: &[String], version: &str) -> ModelCatalog {
    ModelCatalog {
        provider: AgentProvider::ClaudeCode,
        models: aliases
            .iter()
            .map(|a| ModelOption {
                id: a.clone(),
                display_name: format!("{a} (alias for the latest)"),
                is_default: false,
            })
            .collect(),
        accepts_full_names: true,
        source: format!("aliases named by `claude --help` ({version})"),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const HELP: &str = "Options:\n  --mcp-config <configs...>             Load MCP servers\n  --model <model>                       \
        Model for the current session. Provide\n                                        an alias for the latest model (e.g.\n  \
                                      'fable', 'opus', or 'sonnet') or a\n                                        model's full name.\n  \
        -n, --name <name>                     Set a display name ('x' here is not an alias)\n";

    #[test]
    fn aliases_come_from_the_model_option_only() {
        assert_eq!(claude_aliases_from_help(HELP), ["fable", "opus", "sonnet"]);
        assert!(claude_aliases_from_help("no such option").is_empty());
    }

    #[test]
    fn claude_validation_accepts_aliases_and_full_names_only() {
        let c = claude_catalog(&claude_aliases_from_help(HELP), "2.1.325");
        assert!(c.validate(None).is_ok());
        assert!(c.validate(Some("opus")).is_ok());
        assert!(c.validate(Some("claude-sonnet-4-5")).is_ok());
        let err = c.validate(Some("gpt-6-luna")).unwrap_err();
        assert!(err.contains("does not offer") && err.contains("fable, opus, sonnet"), "{err}");
        assert!(c.validate(Some("claude-")).is_err());
        assert!(c.validate(Some("opus --dangerous")).is_err());
    }

    #[test]
    fn listed_models_only_when_full_names_are_not_accepted() {
        let c = ModelCatalog {
            provider: AgentProvider::Codex,
            models: vec![ModelOption {
                id: "gpt-6-luna".into(),
                display_name: "GPT-6-Luna".into(),
                is_default: true,
            }],
            accepts_full_names: false,
            source: "model/list".into(),
            error: None,
        };
        assert!(c.validate(Some("gpt-6-luna")).is_ok());
        assert!(c.validate(Some("claude-opus")).is_err());
        let unreadable = ModelCatalog { models: vec![], error: Some("app-server failed".into()), ..c };
        assert!(unreadable.validate(Some("gpt-6-luna")).unwrap_err().contains("cannot check"));
        assert!(unreadable.validate(None).is_ok());
    }
}
