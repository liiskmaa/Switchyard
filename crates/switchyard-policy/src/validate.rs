//! Rule validation. Applied when a rule is created or updated, and again by
//! the evaluator (a rule that fails validation is ignored, never trusted).

use crate::model::{Conditions, Decision, OperationCategory, Rule};

pub const MAX_PATH_PREFIX_BYTES: usize = 512;
pub const MAX_COMMAND_BYTES: usize = 512;
pub const MAX_DESCRIPTION_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// The rule is malformed (bad condition, missing description, …).
    #[error("{0}")]
    Invalid(String),
    /// The combination is not supported (e.g. `allow` for network access).
    #[error("{0}")]
    Unsupported(String),
}

fn invalid(m: impl Into<String>) -> RuleError {
    RuleError::Invalid(m.into())
}

/// Validates a rule's category, decision, conditions and description.
pub fn validate_rule(
    category: OperationCategory,
    decision: Decision,
    conditions: &Conditions,
    description: &str,
) -> Result<(), RuleError> {
    if !category.configurable() {
        return Err(RuleError::Unsupported(
            "unrecognized operations are always blocked; rules cannot target them".into(),
        ));
    }
    if decision == Decision::Allow && !category.allow_supported() {
        return Err(RuleError::Unsupported(format!(
            "automatic approval is not supported for {}; use ask or block",
            category.phrase()
        )));
    }
    let description = description.trim();
    if description.is_empty() {
        return Err(invalid("a description is required"));
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(invalid(format!("the description is longer than {MAX_DESCRIPTION_CHARS} characters")));
    }
    if description.chars().any(|c| c.is_control()) {
        return Err(invalid("the description contains control characters"));
    }
    if let Some(prefix) = &conditions.path_prefix {
        if !category.is_path_category() {
            return Err(invalid(format!("a path condition does not apply to {}", category.phrase())));
        }
        validate_path_prefix(prefix)?;
    }
    match &conditions.command {
        Some(command) => {
            if category != OperationCategory::Shell {
                return Err(invalid(format!("a command condition does not apply to {}", category.phrase())));
            }
            validate_command(command)?;
        }
        None if category == OperationCategory::Shell && decision == Decision::Allow => {
            return Err(RuleError::Unsupported(
                "shell commands can only be allowed automatically one exact command at a time".into(),
            ));
        }
        None => {}
    }
    Ok(())
}

/// Re-validates a stored rule (used by the evaluator).
pub fn validate_stored(rule: &Rule) -> Result<(), RuleError> {
    validate_rule(rule.category, rule.decision, &rule.conditions, &rule.description)
}

/// A relative, normalized path inside the workspace.
pub fn validate_path_prefix(prefix: &str) -> Result<(), RuleError> {
    if prefix.is_empty() {
        return Err(invalid("the path must not be empty (omit it to match the whole workspace)"));
    }
    if prefix.len() > MAX_PATH_PREFIX_BYTES {
        return Err(invalid(format!("the path is longer than {MAX_PATH_PREFIX_BYTES} bytes")));
    }
    if prefix.chars().any(|c| c.is_control()) {
        return Err(invalid("the path contains control characters"));
    }
    if prefix.starts_with('/') || prefix.starts_with('~') {
        return Err(invalid("the path must be relative to the workspace"));
    }
    let trimmed = prefix.strip_suffix('/').unwrap_or(prefix);
    for part in trimmed.split('/') {
        match part {
            "" => return Err(invalid("the path contains an empty component")),
            "." | ".." => return Err(invalid("the path must not contain . or .. components")),
            _ => {}
        }
    }
    Ok(())
}

/// Normalized form of a valid path prefix (no trailing slash).
pub fn normalize_path_prefix(prefix: &str) -> String {
    prefix.strip_suffix('/').unwrap_or(prefix).to_owned()
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:@%+,=-".contains(c)
}

/// An exact command made only of plain words (see ADR 0016).
pub fn validate_command(command: &str) -> Result<(), RuleError> {
    if command.is_empty() {
        return Err(invalid("the command must not be empty"));
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err(invalid(format!("the command is longer than {MAX_COMMAND_BYTES} bytes")));
    }
    let words: Vec<&str> = command.split(' ').collect();
    if words.iter().any(|w| w.is_empty()) {
        return Err(invalid("separate words with single spaces, without leading or trailing spaces"));
    }
    for (i, word) in words.iter().enumerate() {
        if let Some(bad) = word.chars().find(|c| !word_char(*c)) {
            return Err(invalid(format!(
                "{bad:?} is not allowed in an exact command: only letters, digits, spaces and _ . / : @ % + , = - \
                 (no quotes, variables, globs, redirection, pipes or command separators)"
            )));
        }
        if word.starts_with('=') {
            return Err(invalid("a word must not start with ="));
        }
        if i == 0 {
            if word.contains('=') {
                return Err(invalid("the command must not start with a variable assignment"));
            }
            if !word.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '/') {
                return Err(invalid("the command must start with a program name"));
            }
        }
    }
    Ok(())
}

/// Two active rules with the same scope, category and conditions conflict.
pub fn same_match(a: &Rule, b: &Rule) -> bool {
    a.project_id == b.project_id
        && a.workspace_id == b.workspace_id
        && a.category == b.category
        && a.conditions.path_prefix.as_deref().map(normalize_path_prefix)
            == b.conditions.path_prefix.as_deref().map(normalize_path_prefix)
        && a.conditions.command == b.conditions.command
}

#[cfg(test)]
mod tests {
    use super::*;
    use OperationCategory::*;

    fn cond(path: Option<&str>, command: Option<&str>) -> Conditions {
        Conditions { path_prefix: path.map(Into::into), command: command.map(Into::into) }
    }

    #[test]
    fn commands_are_plain_words_only() {
        for ok in
            ["cargo test", "npm run lint", "git status --porcelain=v2", "./build.sh", "make -j4 all", "ls"]
        {
            assert_eq!(validate_command(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            " cargo test",
            "cargo  test",
            "cargo test ",
            "cargo test; rm -rf /",
            "cargo test && rm x",
            "echo $HOME",
            "echo `id`",
            "cat x | sh",
            "cat > x",
            "ls *",
            "ls ~",
            "FOO=1 cargo test",
            "ls =ls",
            "echo 'a'",
            "echo \"a\"",
            "a\\ b",
            "ls\ttab",
            "ls\nrm",
            "echo ä",
            "-rf x",
            "echo {a,b}",
            "ls #x",
            "!x",
        ] {
            assert!(validate_command(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(validate_command(&"a".repeat(MAX_COMMAND_BYTES + 1)).is_err());
    }

    #[test]
    fn path_prefixes_are_relative_and_normalized() {
        for ok in ["docs", "docs/", "src/generated", ".github", "a b/ä"] {
            assert_eq!(validate_path_prefix(ok), Ok(()), "{ok}");
        }
        for bad in ["", "/etc", "~/x", "../x", "a/../b", "./a", "a//b", "a/./b", "a\0b", "a\nb"] {
            assert!(validate_path_prefix(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(normalize_path_prefix("docs/"), "docs");
    }

    #[test]
    fn rule_shapes() {
        let ok = |c, d, cd: Conditions| validate_rule(c, d, &cd, "why");
        assert!(ok(FileEdit, Decision::Allow, cond(Some("docs"), None)).is_ok());
        assert!(ok(FileRead, Decision::Deny, cond(None, None)).is_ok());
        assert!(ok(Shell, Decision::Allow, cond(None, Some("cargo test"))).is_ok());
        assert!(ok(Shell, Decision::Deny, cond(None, None)).is_ok());
        assert!(ok(Network, Decision::Deny, cond(None, None)).is_ok());
        // Broad shell allow, allow for unsupported categories, unknown.
        assert!(matches!(ok(Shell, Decision::Allow, cond(None, None)), Err(RuleError::Unsupported(_))));
        assert!(matches!(ok(Network, Decision::Allow, cond(None, None)), Err(RuleError::Unsupported(_))));
        assert!(matches!(
            ok(ExternalTool, Decision::Allow, cond(None, None)),
            Err(RuleError::Unsupported(_))
        ));
        assert!(matches!(ok(Unknown, Decision::Deny, cond(None, None)), Err(RuleError::Unsupported(_))));
        // Conditions must fit the category.
        assert!(ok(Shell, Decision::Ask, cond(Some("docs"), None)).is_err());
        assert!(ok(FileRead, Decision::Ask, cond(None, Some("ls"))).is_err());
        assert!(ok(Shell, Decision::Allow, cond(None, Some("cargo test; id"))).is_err());
        // Description is required.
        assert!(validate_rule(FileRead, Decision::Ask, &Conditions::default(), "  ").is_err());
        assert!(validate_rule(FileRead, Decision::Ask, &Conditions::default(), "a\u{7}").is_err());
    }

    #[test]
    fn unknown_condition_fields_are_rejected() {
        let parsed: Result<Conditions, _> = serde_json::from_str(r#"{"path_prefix":"a","script":"x"}"#);
        assert!(parsed.is_err());
    }
}
