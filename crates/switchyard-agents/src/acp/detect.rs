//! Locating and probing the installed Gemini CLI.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

/// Overrides the executable path.
pub const ENV_GEMINI_BIN: &str = "SWITCHYARD_GEMINI_BIN";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectError {
    #[error("Gemini CLI not found (set {ENV_GEMINI_BIN} or install `gemini`)")]
    NotFound,
    #[error("{path} cannot be executed: {message}")]
    NotExecutable { path: String, message: String },
    #[error("unexpected `gemini --version` output: {output:?}")]
    UnrecognizedVersion { output: String },
    #[error("Gemini CLI {version} has no `--acp` mode")]
    Unsupported { version: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeminiInstallation {
    pub executable: PathBuf,
    pub version: String,
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Resolution order: explicit path, `SWITCHYARD_GEMINI_BIN`, `PATH`, then
/// common install locations (GUI apps on macOS get a minimal `PATH`).
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(ENV_GEMINI_BIN).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join("gemini"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let fallbacks = home
        .iter()
        .map(|h| h.join(".local/bin/gemini"))
        .chain([PathBuf::from("/opt/homebrew/bin/gemini"), PathBuf::from("/usr/local/bin/gemini")]);
    from_path.chain(fallbacks).find(|p| is_executable(p))
}

async fn run(executable: &Path, args: &[&str]) -> Result<(bool, String), DetectError> {
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
    Ok((output.status.success(), String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// `0.70.0` (the whole output of `gemini --version`).
pub(crate) fn parse_version(output: &str) -> Option<String> {
    let token = output.split_whitespace().next()?;
    let valid = token.split('.').count() >= 2
        && token.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    valid.then(|| token.to_owned())
}

/// Locates the CLI, reads its version and checks for `--acp`.
pub async fn detect(explicit: Option<&Path>) -> Result<GeminiInstallation, DetectError> {
    let executable = locate(explicit).ok_or(DetectError::NotFound)?;
    if !is_executable(&executable) {
        return Err(DetectError::NotExecutable {
            path: executable.display().to_string(),
            message: "not an executable file".into(),
        });
    }
    let (ok, out) = run(&executable, &["--version"]).await?;
    let version = parse_version(&out)
        .filter(|_| ok)
        .ok_or(DetectError::UnrecognizedVersion { output: out.trim().into() })?;
    let (ok, help) = run(&executable, &["--help"]).await?;
    if !ok || !help.contains("--acp") || !help.contains("--admin-policy") {
        return Err(DetectError::Unsupported { version });
    }
    Ok(GeminiInstallation { executable, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.70.0\n").as_deref(), Some("0.70.0"));
        assert_eq!(parse_version("gemini 1.0").as_deref(), None);
        assert_eq!(parse_version("").as_deref(), None);
    }
}
