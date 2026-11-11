use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::error::GitError;

#[derive(Debug, Clone)]
pub struct GitConfig {
    /// Executable to run; resolved through `PATH` when not absolute.
    pub binary: PathBuf,
    /// Upper bound for any single git invocation.
    pub timeout: Duration,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self { binary: PathBuf::from("git"), timeout: Duration::from_secs(15) }
    }
}

/// Handle for running read-only git commands.
#[derive(Debug, Clone, Default)]
pub struct Git {
    config: GitConfig,
}

/// Environment variables that would redirect git away from `-C <dir>`.
const REDIRECTING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

pub(crate) struct Output {
    pub success: bool,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

impl Git {
    pub fn new(config: GitConfig) -> Self {
        Self { config }
    }

    /// Runs `git -C <dir> <args>` without a terminal, with optional locks
    /// disabled so inspection never contends with a concurrent writer.
    pub(crate) async fn run(&self, dir: &Path, args: &[&str]) -> Result<Output, GitError> {
        self.run_with_input(dir, args, None).await
    }

    /// Like [`Self::run`], writing `input` to the child's stdin.
    pub(crate) async fn run_with_input(
        &self,
        dir: &Path,
        args: &[&str],
        input: Option<Vec<u8>>,
    ) -> Result<Output, GitError> {
        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("--no-optional-locks")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        for var in REDIRECTING_ENV {
            cmd.env_remove(var);
        }
        let joined = args.join(" ");
        let mut child = cmd
            .spawn()
            .map_err(|source| GitError::Unavailable { binary: self.config.binary.clone(), source })?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            use tokio::io::AsyncWriteExt;
            // Written concurrently with reading output, so large inputs
            // cannot deadlock on full pipes.
            tokio::spawn(async move {
                if let Err(err) = stdin.write_all(&bytes).await {
                    tracing::debug!(%err, "writing git stdin failed");
                }
            });
        }
        let output = tokio::time::timeout(self.config.timeout, child.wait_with_output())
            .await
            .map_err(|_| GitError::Timeout {
                args: joined.clone(),
                timeout_ms: self.config.timeout.as_millis(),
            })?
            .map_err(|source| GitError::Io { path: dir.to_path_buf(), source })?;
        tracing::debug!(args = %joined, dir = %dir.display(), status = %output.status, "git");
        Ok(Output {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    pub(crate) fn failed(args: &[&str], out: Output) -> GitError {
        GitError::Failed { args: args.join(" "), status: out.status, stderr: out.stderr }
    }
}
