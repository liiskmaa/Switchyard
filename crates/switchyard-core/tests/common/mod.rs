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
}

pub fn data_dir() -> tempfile::TempDir {
    // Short prefix: Unix socket paths are limited to ~104 bytes on macOS.
    tempfile::Builder::new().prefix("sy").tempdir().unwrap()
}

pub fn config_for(dir: &Path) -> CoreConfig {
    CoreConfig::for_data_dir(dir.join("data"), None).unwrap()
}

impl TestCore {
    pub async fn start() -> Self {
        let dir = data_dir();
        let core = switchyard_core::start(config_for(dir.path())).await.unwrap();
        Self { dir, core: Some(core) }
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
        self.core = Some(switchyard_core::start(config_for(self.dir.path())).await.unwrap());
    }
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
