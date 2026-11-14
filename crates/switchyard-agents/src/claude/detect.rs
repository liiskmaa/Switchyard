//! Locating and probing the installed Claude Code CLI.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

use super::REQUIRED_FLAGS;

/// Overrides the executable path.
pub const ENV_CLAUDE_BIN: &str = "SWITCHYARD_CLAUDE_BIN";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectError {
    #[error("Claude Code CLI not found (set {ENV_CLAUDE_BIN} or install `claude`)")]
    NotFound,
    #[error("{path} cannot be executed: {message}")]
    NotExecutable { path: String, message: String },
    #[error("unexpected `claude --version` output: {output:?}")]
    UnrecognizedVersion { output: String },
    #[error("Claude Code {version} lacks required flags: {missing:?}")]
    Unsupported { version: String, missing: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaudeInstallation {
    pub executable: PathBuf,
    pub version: String,
    /// The `--model` aliases the installed CLI's `--help` names.
    pub model_aliases: Vec<String>,
}

/// Login state as reported by `claude auth status --json`. Only these
/// non-sensitive fields are read; account identifiers are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub method: Option<String>,
    pub provider: Option<String>,
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Resolution order: explicit path, `SWITCHYARD_CLAUDE_BIN`, `PATH`, then
/// common install locations (GUI apps on macOS get a minimal `PATH`).
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(ENV_CLAUDE_BIN).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join("claude"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let fallbacks = home
        .iter()
        .flat_map(|h| [h.join(".local/bin/claude"), h.join(".claude/local/claude")])
        .chain([PathBuf::from("/opt/homebrew/bin/claude"), PathBuf::from("/usr/local/bin/claude")]);
    from_path.chain(fallbacks).find(|p| is_executable(p))
}

async fn run(executable: &Path, args: &[&str]) -> Result<String, DetectError> {
    let not_exec =
        |message: String| DetectError::NotExecutable { path: executable.display().to_string(), message };
    let child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| not_exec(e.to_string()))?;
    let output = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| not_exec(format!("`{}` timed out", args.join(" "))))?
        .map_err(|e| not_exec(e.to_string()))?;
    if !output.status.success() {
        return Err(not_exec(format!("`{}` exited with {}", args.join(" "), output.status)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses `2.1.322 (Claude Code)` → `2.1.322`.
pub(crate) fn parse_version(output: &str) -> Option<String> {
    let token = output.split_whitespace().next()?;
    let valid = token.split('.').count() >= 2
        && token.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    valid.then(|| token.to_owned())
}

/// Locates the CLI, reads its version and checks required flags.
pub async fn detect(explicit: Option<&Path>) -> Result<ClaudeInstallation, DetectError> {
    let executable = locate(explicit).ok_or(DetectError::NotFound)?;
    if !is_executable(&executable) {
        return Err(DetectError::NotExecutable {
            path: executable.display().to_string(),
            message: "not an executable file".into(),
        });
    }
    let raw = run(&executable, &["--version"]).await?;
    let version =
        parse_version(&raw).ok_or(DetectError::UnrecognizedVersion { output: raw.trim().into() })?;
    let help = run(&executable, &["--help"]).await?;
    let missing = missing_flags(&help);
    if !missing.is_empty() {
        return Err(DetectError::Unsupported { version, missing });
    }
    let model_aliases = crate::models::claude_aliases_from_help(&help);
    Ok(ClaudeInstallation { executable, version, model_aliases })
}

/// Required flags not mentioned in `help`. Some flags the adapter relies on
/// (e.g. `--permission-prompt-tool` in 2.1.322) are not listed as options and
/// only appear inside other flags' descriptions, so any mention counts. A CLI
/// that then rejects a flag fails at launch, which is reported with stderr.
pub(crate) fn missing_flags(help: &str) -> Vec<String> {
    let tokens: std::collections::HashSet<&str> = help
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| w.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')))
        .collect();
    REQUIRED_FLAGS.iter().filter(|f| !tokens.contains(**f)).map(|f| (*f).to_owned()).collect()
}

/// Reads login state without exposing credentials or account identity.
pub async fn auth_status(installation: &ClaudeInstallation) -> Result<AuthStatus, DetectError> {
    let raw = run(&installation.executable, &["auth", "status", "--json"]).await?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| DetectError::UnrecognizedVersion { output: "auth status: not JSON".into() })?;
    let field = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    Ok(AuthStatus {
        logged_in: json.get("loggedIn").and_then(serde_json::Value::as_bool).unwrap_or(false),
        method: field("authMethod"),
        provider: field("apiProvider"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("2.1.322 (Claude Code)\n").as_deref(), Some("2.1.322"));
        assert_eq!(parse_version("claude 2.1").as_deref(), None);
        assert_eq!(parse_version("").as_deref(), None);
    }

    #[test]
    fn flags_mentioned_only_in_descriptions_count() {
        let help = "--input-format <f>\n --output-format\n --session-id\n --resume, -r\n \
                    --permission-prompts <t> (the SDK host or\n --permission-prompt-tool)\n --restricted\n \
                    --strict-mcp-config\n --include-partial-messages\n --tools\n --settings";
        assert!(missing_flags(help).is_empty(), "{:?}", missing_flags(help));
        assert_eq!(
            missing_flags("--input-format"),
            REQUIRED_FLAGS[1..].iter().map(|f| f.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn explicit_path_wins() {
        assert_eq!(locate(Some(Path::new("/x/claude"))), Some(PathBuf::from("/x/claude")));
    }
}
