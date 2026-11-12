//! Locating and probing the installed Codex CLI.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

/// Overrides the executable path.
pub const ENV_CODEX_BIN: &str = "SWITCHYARD_CODEX_BIN";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectError {
    #[error("Codex CLI not found (set {ENV_CODEX_BIN} or install `codex`)")]
    NotFound,
    #[error("{path} cannot be executed: {message}")]
    NotExecutable { path: String, message: String },
    #[error("unexpected `codex --version` output: {output:?}")]
    UnrecognizedVersion { output: String },
    #[error("Codex {version} has no usable `app-server` (stdio transport)")]
    Unsupported { version: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexInstallation {
    pub executable: PathBuf,
    pub version: String,
}

/// Login state from `codex login status`. Only the method is kept; the
/// output never contains credentials, and nothing else is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub method: Option<String>,
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Resolution order: explicit path, `SWITCHYARD_CODEX_BIN`, `PATH`, then
/// common install locations (GUI apps on macOS get a minimal `PATH`).
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(ENV_CODEX_BIN).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join("codex"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let fallbacks = home
        .iter()
        .map(|h| h.join(".local/bin/codex"))
        .chain([PathBuf::from("/opt/homebrew/bin/codex"), PathBuf::from("/usr/local/bin/codex")]);
    from_path.chain(fallbacks).find(|p| is_executable(p))
}

/// Runs a probe and returns (success, stdout, stderr).
async fn run(executable: &Path, args: &[&str]) -> Result<(bool, String, String), DetectError> {
    let not_exec =
        |message: String| DetectError::NotExecutable { path: executable.display().to_string(), message };
    let child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| not_exec(e.to_string()))?;
    let output = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| not_exec(format!("`{}` timed out", args.join(" "))))?
        .map_err(|e| not_exec(e.to_string()))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Parses `codex-cli 0.166.0` → `0.166.0`.
pub(crate) fn parse_version(output: &str) -> Option<String> {
    let token = output.split_whitespace().find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let valid = token.split('.').count() >= 2
        && token.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    (valid && output.trim_start().starts_with("codex")).then(|| token.to_owned())
}

/// `app-server` with the stdio transport, as `codex app-server --help` lists it.
pub(crate) fn supports_app_server(help: &str) -> bool {
    help.contains("--listen") && help.contains("stdio://")
}

/// Locates the CLI, reads its version and checks for `app-server`.
pub async fn detect(explicit: Option<&Path>) -> Result<CodexInstallation, DetectError> {
    let executable = locate(explicit).ok_or(DetectError::NotFound)?;
    if !is_executable(&executable) {
        return Err(DetectError::NotExecutable {
            path: executable.display().to_string(),
            message: "not an executable file".into(),
        });
    }
    let (ok, out, _) = run(&executable, &["--version"]).await?;
    let version = parse_version(&out)
        .filter(|_| ok)
        .ok_or(DetectError::UnrecognizedVersion { output: out.trim().into() })?;
    let (ok, help, _) = run(&executable, &["app-server", "--help"]).await?;
    if !ok || !supports_app_server(&help) {
        return Err(DetectError::Unsupported { version });
    }
    Ok(CodexInstallation { executable, version })
}

/// Parses `codex login status` output ("Logged in using ChatGPT",
/// "Logged in using an API key - …", "Not logged in").
pub(crate) fn parse_login_status(success: bool, text: &str) -> AuthStatus {
    let line =
        text.lines().map(str::trim).find(|l| l.starts_with("Logged in") || l.starts_with("Not logged in"));
    match line {
        Some(l) if success && l.starts_with("Logged in") => {
            let method = if l.contains("ChatGPT") {
                Some("chatgpt")
            } else if l.contains("API key") {
                Some("api_key")
            } else {
                None
            };
            AuthStatus { logged_in: true, method: method.map(str::to_owned) }
        }
        _ => AuthStatus { logged_in: false, method: None },
    }
}

/// Reads login state without exposing credentials or account identity.
pub async fn auth_status(installation: &CodexInstallation) -> Result<AuthStatus, DetectError> {
    let (ok, out, err) = run(&installation.executable, &["login", "status"]).await?;
    Ok(parse_login_status(ok, &format!("{out}\n{err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("codex-cli 0.166.0\n").as_deref(), Some("0.166.0"));
        assert_eq!(parse_version("2.1.322 (Claude Code)"), None);
        assert_eq!(parse_version("codex-cli dev"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn login_status_parsing_keeps_only_the_method() {
        assert_eq!(
            parse_login_status(true, "Logged in using ChatGPT\n"),
            AuthStatus { logged_in: true, method: Some("chatgpt".into()) }
        );
        let api = parse_login_status(true, "Logged in using an API key - sk-proj-***abc");
        assert_eq!(api, AuthStatus { logged_in: true, method: Some("api_key".into()) });
        assert!(!parse_login_status(false, "Not logged in").logged_in);
        assert!(
            !parse_login_status(false, "Logged in using ChatGPT").logged_in,
            "a failing command is not logged in"
        );
    }

    #[test]
    fn app_server_help() {
        assert!(supports_app_server("--listen <URL>  Supported values: `stdio://` (default)"));
        assert!(!supports_app_server("Usage: codex exec"));
    }
}
