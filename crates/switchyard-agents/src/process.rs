//! Supervised child processes.
//!
//! Agents are launched directly (never through a shell), with an explicitly
//! constructed environment, piped stdio, in their own process group, and
//! `kill_on_drop` so that a dropped handle can never leave an orphan.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::process::{Child, Command};

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("child process has no {0} pipe")]
    MissingPipe(&'static str),
    #[error("cannot signal process {pid}: {message}")]
    Signal { pid: u32, message: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// What to launch. Arguments are passed to `execve` verbatim.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

/// Rules for building a child environment from the parent's.
#[derive(Debug, Clone, Default)]
pub struct EnvPolicy {
    /// Exact variable names passed through.
    pub allow: &'static [&'static str],
    /// Name prefixes passed through.
    pub allow_prefixes: &'static [&'static str],
    /// Exact names or prefixes (ending in `*`) never passed, even if allowed.
    pub deny: &'static [&'static str],
}

impl EnvPolicy {
    fn denied(&self, name: &str) -> bool {
        self.deny.iter().any(|d| match d.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == *d,
        })
    }

    fn allowed(&self, name: &str) -> bool {
        self.allow.contains(&name) || self.allow_prefixes.iter().any(|p| name.starts_with(p))
    }

    /// Filters `parent` (typically `std::env::vars_os()`). Non-UTF-8 names are
    /// dropped.
    pub fn build(&self, parent: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
        let mut env: Vec<_> = parent
            .into_iter()
            .filter(|(k, _)| k.to_str().is_some_and(|name| self.allowed(name) && !self.denied(name)))
            .collect();
        env.sort();
        env
    }
}

/// Prepends `dir` to the `PATH` entry of `env` (adding `PATH` if missing).
pub fn prepend_path(env: &mut Vec<(OsString, OsString)>, dir: &Path) {
    let dir = dir.as_os_str().to_owned();
    match env.iter_mut().find(|(k, _)| k == OsStr::new("PATH")) {
        Some((_, value)) => {
            let already = std::env::split_paths(value).any(|p| p.as_os_str() == dir);
            if !already {
                let mut joined = dir;
                joined.push(":");
                joined.push(&*value);
                *value = joined;
            }
        }
        None => env.push((OsString::from("PATH"), dir)),
    }
}

/// Spawns `spec` with piped stdio in a new process group.
pub fn spawn(spec: &ProcessSpec) -> Result<Child, ProcessError> {
    Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group: terminal signals aimed at Core don't reach the
        // agent, and Core controls its shutdown explicitly.
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ProcessError::Spawn { program: spec.program.clone(), source })
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl From<ExitStatus> for ExitInfo {
    fn from(status: ExitStatus) -> Self {
        Self { code: status.code(), signal: status.signal() }
    }
}

impl ExitInfo {
    pub fn describe(&self) -> String {
        match (self.code, self.signal) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "unknown exit status".to_owned(),
        }
    }
}

/// Sends `signal` to `pid`.
pub fn send_signal(pid: u32, signal: Signal) -> Result<(), ProcessError> {
    let raw =
        i32::try_from(pid).map_err(|_| ProcessError::Signal { pid, message: "pid out of range".into() })?;
    kill(Pid::from_raw(raw), signal).map_err(|e| ProcessError::Signal { pid, message: e.to_string() })
}

/// Waits up to `grace` for exit, then SIGTERM, waits `term_grace`, then
/// SIGKILL. Always reaps the child.
pub async fn terminate(child: &mut Child, grace: Duration, term_grace: Duration) -> ExitInfo {
    if let Ok(Ok(status)) = tokio::time::timeout(grace, child.wait()).await {
        return status.into();
    }
    if let Some(pid) = child.id() {
        tracing::warn!(pid, "agent did not exit in time; sending SIGTERM");
        if let Err(err) = send_signal(pid, Signal::SIGTERM) {
            tracing::warn!(%err, "SIGTERM failed");
        }
        if let Ok(Ok(status)) = tokio::time::timeout(term_grace, child.wait()).await {
            return status.into();
        }
        tracing::warn!(pid, "agent ignored SIGTERM; sending SIGKILL");
    }
    if let Err(err) = child.start_kill() {
        tracing::error!(%err, "SIGKILL failed");
    }
    match child.wait().await {
        Ok(status) => status.into(),
        Err(err) => {
            tracing::error!(%err, "cannot reap agent process");
            ExitInfo { code: None, signal: Some(Signal::SIGKILL as i32) }
        }
    }
}

/// One line read from a child's output.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Complete(Vec<u8>),
    /// The line exceeded the limit. Its content was discarded up to the next
    /// newline so the stream stays aligned.
    TooLong {
        bytes: usize,
    },
}

/// Reads one line with a hard memory bound. `Ok(None)` at end of stream.
/// A final line without a trailing newline is returned as complete.
pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> std::io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut discarded = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(match (line.is_empty(), discarded) {
                (true, 0) => None,
                (_, 0) => Some(Line::Complete(line)),
                (_, n) => Some(Line::TooLong { bytes: n }),
            });
        }
        let (chunk, newline) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (available.len(), false),
        };
        if discarded > 0 || line.len() + chunk > max {
            discarded += line.len() + chunk;
            line.clear();
        } else {
            line.extend_from_slice(&available[..chunk]);
        }
        reader.consume(chunk + usize::from(newline));
        if newline {
            if discarded > 0 {
                return Ok(Some(Line::TooLong { bytes: discarded }));
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(Line::Complete(line)));
        }
    }
}

/// Keeps the last `capacity` bytes written to it (for stderr diagnostics).
#[derive(Debug)]
pub struct TailBuffer {
    data: std::collections::VecDeque<u8>,
    capacity: usize,
}

impl TailBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { data: std::collections::VecDeque::with_capacity(capacity), capacity }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for b in bytes {
            if self.data.len() == self.capacity {
                self.data.pop_front();
            }
            self.data.push_back(*b);
        }
    }

    pub fn contents(&self) -> String {
        let bytes: Vec<u8> = self.data.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn bounded_lines_resync_after_oversized_line() {
        let input = b"short\nthis line is far too long\nok\nlast-no-newline";
        let mut r = BufReader::with_capacity(4, &input[..]);
        assert_eq!(read_line(&mut r, 10).await.unwrap(), Some(Line::Complete(b"short".to_vec())));
        assert!(matches!(read_line(&mut r, 10).await.unwrap(), Some(Line::TooLong { .. })));
        assert_eq!(read_line(&mut r, 10).await.unwrap(), Some(Line::Complete(b"ok".to_vec())));
        assert_eq!(read_line(&mut r, 20).await.unwrap(), Some(Line::Complete(b"last-no-newline".to_vec())));
        assert_eq!(read_line(&mut r, 10).await.unwrap(), None);
    }

    #[test]
    fn env_policy_filters_and_denies() {
        let policy = EnvPolicy {
            allow: &["HOME", "PATH", "CLAUDECODE"],
            allow_prefixes: &["ANTHROPIC_", "CLAUDE_"],
            deny: &["CLAUDECODE", "CLAUDE_CODE_SESSION*"],
        };
        let parent = [
            ("HOME", "/h"),
            ("PATH", "/bin"),
            ("SECRET_TOKEN", "x"),
            ("ANTHROPIC_API_KEY", "k"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "s"),
            ("CLAUDE_CONFIG_DIR", "/c"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let names: Vec<String> =
            policy.build(parent).into_iter().map(|(k, _)| k.into_string().unwrap()).collect();
        assert_eq!(names, ["ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR", "HOME", "PATH"]);
    }

    #[test]
    fn prepend_path_is_idempotent() {
        let mut env = vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))];
        prepend_path(&mut env, Path::new("/opt/claude/bin"));
        prepend_path(&mut env, Path::new("/opt/claude/bin"));
        assert_eq!(env[0].1, OsString::from("/opt/claude/bin:/usr/bin:/bin"));
    }

    #[test]
    fn tail_buffer_keeps_last_bytes() {
        let mut t = TailBuffer::new(5);
        t.push(b"hello world");
        assert_eq!(t.contents(), "world");
    }
}
