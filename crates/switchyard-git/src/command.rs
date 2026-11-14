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
    /// Upper bound for invocations that check out a tree (`worktree add`).
    pub checkout_timeout: Duration,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("git"),
            timeout: Duration::from_secs(15),
            checkout_timeout: Duration::from_secs(300),
        }
    }
}

/// Handle for running git commands. Read-only, except worktree add/remove
/// and the integration operations in [`crate::integrate`].
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
    /// The exit code; `None` if Git was killed by a signal.
    pub code: Option<i32>,
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
        self.exec(dir, args, input, self.config.timeout, &[]).await
    }

    /// Runs a command that changes the repository. Hooks are disabled
    /// (`core.hooksPath=/dev/null`): Core never runs repository scripts as a
    /// side effect of an integration step (ADR 0029).
    pub(crate) async fn run_write(&self, dir: &Path, args: &[&str], slow: bool) -> Result<Output, GitError> {
        let mut full = vec!["-c", "core.hooksPath=/dev/null"];
        full.extend_from_slice(args);
        let timeout =
            if slow { self.config.checkout_timeout.max(self.config.timeout) } else { self.config.timeout };
        self.exec(dir, &full, None, timeout, &[]).await
    }

    /// Like [`Self::run`] with extra environment variables (set after the
    /// redirecting variables are removed), for a private index file.
    pub(crate) async fn run_env(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output, GitError> {
        self.exec(dir, args, None, self.config.checkout_timeout.max(self.config.timeout), env).await
    }

    /// Like [`Self::run`] with a longer bound, for operations that check out
    /// files (worktree creation scales with repository size).
    pub(crate) async fn run_slow(&self, dir: &Path, args: &[&str]) -> Result<Output, GitError> {
        self.exec(dir, args, None, self.config.checkout_timeout.max(self.config.timeout), &[]).await
    }

    async fn exec(
        &self,
        dir: &Path,
        args: &[&str],
        input: Option<Vec<u8>>,
        timeout: Duration,
        env: &[(&str, &str)],
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
        for (k, v) in env {
            cmd.env(k, v);
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
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| GitError::Timeout { args: joined.clone(), timeout_ms: timeout.as_millis() })?
            .map_err(|source| GitError::Io { path: dir.to_path_buf(), source })?;
        tracing::debug!(args = %joined, dir = %dir.display(), status = %output.status, "git");
        Ok(Output {
            success: output.status.success(),
            code: output.status.code(),
            status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    pub(crate) fn failed(args: &[&str], out: Output) -> GitError {
        GitError::Failed { args: args.join(" "), status: out.status, stderr: out.stderr }
    }
}
