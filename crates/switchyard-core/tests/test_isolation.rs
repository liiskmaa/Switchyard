#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Regression tests for test isolation: the default suite can never run a
//! real (paid) Claude Code or Codex CLI. A real CLI is only reachable through
//! `TestCore::start_real` with an exact `=1` opt-in, checked before Core
//! starts.

mod common;

use std::path::PathBuf;

use common::{ENV_REAL_CLAUDE, ENV_REAL_CODEX, FAKE_CLAUDE, FAKE_CODEX, RealCli, TestCore, real_cli_opt_in};
use switchyard_protocol::methods::{AgentsList, AgentsListParams};

#[test]
fn opt_in_must_be_exactly_one() {
    assert!(real_cli_opt_in(Some("1")));
    for bad in [None, Some(""), Some("0"), Some("true"), Some("yes"), Some(" 1"), Some("1 "), Some("11")] {
        assert!(!real_cli_opt_in(bad), "{bad:?}");
    }
}

#[tokio::test]
#[should_panic(expected = "test isolation")]
async fn a_core_without_a_fake_claude_is_refused() {
    TestCore::start_with(|c| c.claude_executable = None).await;
}

#[tokio::test]
#[should_panic(expected = "test isolation")]
async fn a_core_without_a_fake_codex_is_refused() {
    TestCore::start_with(|c| c.codex_executable = None).await;
}

#[tokio::test]
#[should_panic(expected = "test isolation")]
async fn an_installed_cli_path_is_refused_even_if_it_does_not_exist_here() {
    TestCore::start_with(|c| c.codex_executable = Some(PathBuf::from("/usr/local/bin/codex"))).await;
}

#[tokio::test]
#[should_panic(expected = "test isolation")]
async fn a_restart_cannot_switch_to_a_real_cli() {
    let mut core = TestCore::start().await;
    core.restart_with(|c| c.claude_executable = None).await;
}

#[tokio::test]
async fn real_clis_need_the_explicit_opt_in() {
    // Only meaningful when the opt-in is not set for this run.
    if !real_cli_opt_in(std::env::var(ENV_REAL_CLAUDE).ok().as_deref()) {
        assert!(TestCore::start_real(RealCli { claude: true, codex: false }, |_| {}).await.is_none());
    }
    if !real_cli_opt_in(std::env::var(ENV_REAL_CODEX).ok().as_deref()) {
        assert!(TestCore::start_real(RealCli { claude: false, codex: true }, |_| {}).await.is_none());
    }
    assert!(
        TestCore::start_real(RealCli::default(), |_| {}).await.is_none(),
        "nothing requested, nothing real"
    );
}

#[tokio::test]
async fn the_default_core_detects_only_the_fakes() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let agents = client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    let exes: Vec<_> = agents.iter().map(|a| (a.provider.as_str(), a.executable.as_deref())).collect();
    assert_eq!(exes, [("claude_code", Some(FAKE_CLAUDE)), ("codex", Some(FAKE_CODEX))]);
}
