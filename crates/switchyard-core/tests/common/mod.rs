#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]
//! Shared fixtures for Core integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use switchyard_client::{ClientOptions, CoreClient, Notification};
use switchyard_core::{CoreConfig, RunningCore};
use tokio::sync::mpsc;

pub struct TestCore {
    pub dir: tempfile::TempDir,
    pub core: Option<RunningCore>,
    /// Real CLIs this Core may run (only via [`TestCore::start_real`]).
    pub real: RealCli,
}

// --- test isolation: no real (paid) CLI unless explicitly opted in -----------

pub const ENV_REAL_CLAUDE: &str = "SWITCHYARD_REAL_CLAUDE";
pub const ENV_REAL_CODEX: &str = "SWITCHYARD_REAL_CODEX";

/// Which real agent CLIs a test Core may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RealCli {
    pub claude: bool,
    pub codex: bool,
}

/// The opt-in is exactly `1`. Anything else (unset, empty, `true`, ` 1`) is no.
pub fn real_cli_opt_in(value: Option<&str>) -> bool {
    value == Some("1")
}

fn opted_in(var: &str) -> bool {
    real_cli_opt_in(std::env::var(var).ok().as_deref())
}

/// Refuses to start a Core that could run a real agent CLI: every executable
/// must be the fake, a wrapper inside the test's own temporary directory, or a
/// path under `/nonexistent`. Called before Core starts, for every test Core.
pub fn guard(config: &CoreConfig, test_dir: &Path, real: RealCli) {
    fn check(name: &str, exe: Option<&Path>, fake: &str, test_dir: &Path, real_ok: bool) {
        if real_ok {
            return;
        }
        let allowed = exe.is_some_and(|p| {
            p == Path::new(fake) || p.starts_with(test_dir) || p.starts_with("/nonexistent")
        });
        assert!(
            allowed,
            "test isolation: this Core would run a real {name} CLI ({exe:?}). Tests must use the fake \
             CLI; real-CLI tests use TestCore::start_real and an explicit opt-in."
        );
    }
    check("Claude Code", config.claude_executable.as_deref(), FAKE_CLAUDE, test_dir, real.claude);
    check("Codex", config.codex_executable.as_deref(), FAKE_CODEX, test_dir, real.codex);
}

/// Starts Core after [`guard`].
pub async fn start_core(config: CoreConfig, test_dir: &Path, real: RealCli) -> RunningCore {
    guard(&config, test_dir, real);
    switchyard_core::start(config).await.unwrap()
}

pub fn data_dir() -> tempfile::TempDir {
    // Short prefix: Unix socket paths are limited to ~104 bytes on macOS.
    tempfile::Builder::new().prefix("sy").tempdir().unwrap()
}

pub fn config_for(dir: &Path) -> CoreConfig {
    let mut config = CoreConfig::for_data_dir(dir.join("data"), None).unwrap();
    // Tests never run the user's real CLIs; the opt-in real-CLI tests clear
    // these explicitly.
    config.claude_executable = Some(PathBuf::from(FAKE_CLAUDE));
    config.codex_executable = Some(PathBuf::from(FAKE_CODEX));
    config
}

pub const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_switchyard-fake-claude");
pub const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_switchyard-fake-codex");

/// Core configured to run the fake agent instead of a real `claude`.
pub fn agent_config(dir: &Path) -> CoreConfig {
    let mut config = config_for(dir);
    config.claude_executable = Some(PathBuf::from(FAKE_CLAUDE));
    config.codex_executable = Some(PathBuf::from(FAKE_CODEX));
    config.agent_stop_grace = Duration::from_secs(2);
    config.agent_term_grace = Duration::from_secs(2);
    config.file_debounce = Duration::from_millis(200);
    config
}

impl TestCore {
    pub async fn start() -> Self {
        let dir = data_dir();
        let core = start_core(config_for(dir.path()), dir.path(), RealCli::default()).await;
        Self { dir, core: Some(core), real: RealCli::default() }
    }

    pub async fn start_with(configure: impl FnOnce(&mut CoreConfig)) -> Self {
        let dir = data_dir();
        let mut config = agent_config(dir.path());
        configure(&mut config);
        let core = start_core(config, dir.path(), RealCli::default()).await;
        Self { dir, core: Some(core), real: RealCli::default() }
    }

    /// A Core that runs the real CLIs in `real`, only if every one of them is
    /// explicitly opted into (`SWITCHYARD_REAL_CLAUDE=1`, `SWITCHYARD_REAL_CODEX=1`).
    /// Returns `None` (the test is skipped) otherwise, before anything starts.
    pub async fn start_real(real: RealCli, configure: impl FnOnce(&mut CoreConfig)) -> Option<Self> {
        let missing: Vec<&str> = [(real.claude, ENV_REAL_CLAUDE), (real.codex, ENV_REAL_CODEX)]
            .into_iter()
            .filter(|(wanted, var)| *wanted && !opted_in(var))
            .map(|(_, var)| var)
            .collect();
        if !missing.is_empty() || real == RealCli::default() {
            eprintln!("skipped: set {} to exactly 1 to run real CLIs (paid)", missing.join(" and "));
            return None;
        }
        let dir = data_dir();
        let mut config = agent_config(dir.path());
        if real.claude {
            config.claude_executable = None;
        }
        if real.codex {
            config.codex_executable = None;
        }
        configure(&mut config);
        let core = start_core(config, dir.path(), real).await;
        Some(Self { dir, core: Some(core), real })
    }

    /// Stops Core gracefully and starts it again on the same data directory
    /// with the agent configuration plus `configure`.
    pub async fn restart_with(&mut self, configure: impl FnOnce(&mut CoreConfig)) {
        if self.core.is_some() {
            self.stop().await;
        }
        let mut config = agent_config(self.dir.path());
        configure(&mut config);
        guard(&config, self.dir.path(), self.real);
        self.core = Some(start_after_stop(config).await);
    }

    pub fn socket(&self) -> PathBuf {
        config_for(self.dir.path()).paths.socket
    }

    pub async fn connect(&self) -> (CoreClient, mpsc::Receiver<Notification>) {
        connect(&self.socket()).await
    }

    pub async fn stop(&mut self) {
        let core = self.core.take().unwrap();
        core.shutdown("test");
        core.wait().await.unwrap();
    }

    pub async fn restart(&mut self) {
        if self.core.is_some() {
            self.stop().await;
        }
        let config = config_for(self.dir.path());
        guard(&config, self.dir.path(), self.real);
        self.core = Some(start_after_stop(config).await);
    }
}

/// Starts Core right after a previous instance on the same data dir stopped.
/// The instance lock can stay held for a moment after the old Core dropped
/// it: a child process forked concurrently (other tests spawn `git`) holds a
/// copy of the descriptor until it execs. Retry briefly instead of failing.
async fn start_after_stop(config: CoreConfig) -> RunningCore {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match switchyard_core::start(config.clone()).await {
            Ok(core) => return core,
            Err(switchyard_core::CoreError::AlreadyRunning(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("cannot restart Core: {e}"),
        }
    }
}

pub async fn try_connect(
    socket: &Path,
) -> Result<(CoreClient, mpsc::Receiver<Notification>), switchyard_client::ClientError> {
    let mut options = ClientOptions::new("integration-test", "0.0.0-test");
    options.request_timeout = Duration::from_secs(10);
    CoreClient::connect(socket, options).await
}

pub async fn connect(socket: &Path) -> (CoreClient, mpsc::Receiver<Notification>) {
    let mut options = ClientOptions::new("integration-test", "0.0.0");
    options.request_timeout = Duration::from_secs(10);
    CoreClient::connect(socket, options).await.unwrap()
}

pub async fn next_notification(rx: &mut mpsc::Receiver<Notification>) -> Notification {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for notification")
        .expect("notification channel closed")
}

pub fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=Switchyard Test", "-c", "user.email=test@example.invalid"])
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Creates `<parent>/<name>` as a Git repository with one commit.
pub fn git_repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    run_git(&root, &["add", "README.md"]);
    run_git(&root, &["commit", "-q", "-m", "initial"]);
    std::fs::canonicalize(root).unwrap()
}

/// Parses JSON-lines evidence written by a fake CLI. `Ok(None)` when the text
/// ends inside a line (a write in progress); `Err` when any complete line is
/// not valid JSON. Nothing is ever skipped.
pub fn parse_jsonl(text: &str) -> Result<Option<Vec<serde_json::Value>>, String> {
    if !text.is_empty() && !text.ends_with('\n') {
        return Ok(None);
    }
    text.lines()
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}: {l:?}", i + 1)))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Reads JSON-lines evidence strictly. A missing file is no evidence (empty).
/// An unterminated last line is re-read until the writer finishes it (up to
/// 2 s); a malformed complete line, or a line that never completes, fails the test.
pub fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => panic!("cannot read evidence {}: {e}", path.display()),
        };
        match parse_jsonl(&text) {
            Ok(Some(values)) => return values,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => panic!("evidence {} ends in a line that was never completed", path.display()),
            Err(e) => panic!("malformed evidence in {}: {e}", path.display()),
        }
    }
}
