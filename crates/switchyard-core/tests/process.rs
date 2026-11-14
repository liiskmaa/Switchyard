#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Runs the real `switchyard-core` binary as a separate process.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use switchyard_protocol::methods::{
    Empty, ProjectsAdd, ProjectsAddParams, ProjectsList, ShutdownParams, SystemHealth, SystemShutdown,
};

use common::{connect, data_dir, git_repo};

const BIN: &str = env!("CARGO_BIN_EXE_switchyard-core");

fn spawn_core(data: &Path) -> Child {
    Command::new(BIN)
        .arg("--data-dir")
        .arg(data)
        .env("SWITCHYARD_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Waits until Core accepts connections. The socket file alone is not
/// enough: Core binds the socket (creating the file) and then listens, and a
/// connection in between is refused (`ECONNREFUSED`). So this connects, and
/// retries only while the socket is missing or refused.
async fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(_probe) => return,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                assert!(Instant::now() < deadline, "Core did not start accepting connections: {e}");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => panic!("cannot connect to Core's socket: {e}"),
        }
    }
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "Core did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[tokio::test]
async fn core_survives_client_disconnects_and_accepts_reconnection() {
    let dir = data_dir();
    let data = dir.path().join("data");
    let socket = data.join("core.sock");
    let mut child = spawn_core(&data);
    wait_for_socket(&socket).await;

    let (first, _rx) = connect(&socket).await;
    let instance = first.info().core.instance_id;
    let repo = git_repo(dir.path(), "proc");
    first.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap();
    drop(first); // UI goes away without saying goodbye.

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(child.try_wait().unwrap().is_none(), "Core must keep running without clients");

    let (second, _rx) = connect(&socket).await;
    assert_eq!(second.info().core.instance_id, instance, "same Core process");
    assert_eq!(second.call::<ProjectsList>(Empty {}).await.unwrap().projects.len(), 1);
    let health = second.call::<SystemHealth>(Empty {}).await.unwrap();
    assert_eq!(health.core.pid, child.id());
    assert_eq!(health.connected_clients, 1, "the dropped connection was cleaned up");

    let status = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().unwrap();
    assert!(status.success());
    let exit = wait_exit(&mut child);
    assert!(exit.success(), "graceful SIGTERM exit, got {exit:?}");
    assert!(!socket.exists(), "socket removed on shutdown");
    second.closed().await;
}

#[tokio::test]
async fn only_one_core_may_own_a_data_directory() {
    let dir = data_dir();
    let data = dir.path().join("data");
    let mut first = spawn_core(&data);
    wait_for_socket(&data.join("core.sock")).await;

    let mut second = spawn_core(&data);
    let exit = wait_exit(&mut second);
    assert_eq!(exit.code(), Some(3), "second instance refuses to start");

    let (client, _rx) = connect(&data.join("core.sock")).await;
    client.call::<SystemShutdown>(ShutdownParams { reason: Some("test".into()) }).await.unwrap();
    assert!(wait_exit(&mut first).success());
}

#[tokio::test]
async fn stale_socket_from_a_killed_core_is_recovered() {
    let dir = data_dir();
    let data = dir.path().join("data");
    let socket = data.join("core.sock");
    let mut child = spawn_core(&data);
    wait_for_socket(&socket).await;
    child.kill().unwrap(); // SIGKILL: no cleanup runs.
    wait_exit(&mut child);
    assert!(socket.exists(), "stale socket left behind");

    let mut child = spawn_core(&data);
    // Wait until the new Core answers on the replaced socket.
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        let mut options = switchyard_client::ClientOptions::new("t", "0");
        options.request_timeout = Duration::from_secs(5);
        if let Ok((c, _)) = switchyard_client::CoreClient::connect(&socket, options).await {
            break c;
        }
        assert!(Instant::now() < deadline, "restarted Core never became reachable");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(client.call::<SystemHealth>(Empty {}).await.unwrap().core.pid, child.id());
    client.call::<SystemShutdown>(ShutdownParams::default()).await.unwrap();
    assert!(wait_exit(&mut child).success());
}

#[test]
fn version_and_usage_errors() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(switchyard_core::CORE_VERSION));
    let out = Command::new(BIN).arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(BIN).args(["--data-dir", "relative"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test]
async fn core_crash_leaves_no_orphan_agent_and_restart_reports_lost_session() {
    use switchyard_protocol::methods::{
        SessionIdParams, SessionsGet, SessionsResume, SessionsResumeParams, SessionsStart,
        SessionsStartParams, WorkspaceIdParams, WorkspacesCreate, WorkspacesCreateParams, WorkspacesGet,
    };
    use switchyard_protocol::model::{SessionState, WorkspaceStatus};

    let dir = data_dir();
    let data = dir.path().join("data");
    let socket = data.join("core.sock");
    let spawn = || {
        Command::new(BIN)
            .arg("--data-dir")
            .arg(&data)
            .env("SWITCHYARD_LOG", "warn")
            .env("SWITCHYARD_CLAUDE_BIN", common::FAKE_CLAUDE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut core = spawn();
    wait_for_socket(&socket).await;
    let (client, _rx) = connect(&socket).await;
    let repo = git_repo(dir.path(), "crash-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let workspace = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    let session = client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id: workspace.id,
            project_id: None,
            provider: None,
            prompt: None,
            model: None,
        })
        .await
        .unwrap()
        .session;
    std::fs::write(std::path::Path::new(&workspace.path).join("unsaved.txt"), "agent work").unwrap();
    let agent_pid = session.pid.unwrap();
    assert!(pid_alive(agent_pid));

    core.kill().unwrap(); // SIGKILL: no shutdown logic runs.
    wait_exit(&mut core);
    drop(client);
    // The agent's stdin pipe closed with Core, so it exits on its own.
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(agent_pid) {
        assert!(Instant::now() < deadline, "agent process {agent_pid} outlived Core");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mut core = spawn();
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        let mut options = switchyard_client::ClientOptions::new("t", "0");
        options.request_timeout = Duration::from_secs(10);
        if let Ok((c, _)) = switchyard_client::CoreClient::connect(&socket, options).await {
            break c;
        }
        assert!(Instant::now() < deadline, "restarted Core never became reachable");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let after = client.call::<SessionsGet>(SessionIdParams { session_id: session.id }).await.unwrap().session;
    assert_eq!(after.state, SessionState::Failed);
    assert!(after.failure.as_deref().unwrap_or_default().starts_with("core_restarted"));
    assert!(!after.attached);
    // The workspace and its uncommitted work survive the crash; it is no
    // longer marked active, and nothing was deleted or restarted.
    let ws = client.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: workspace.id }).await.unwrap();
    assert_eq!(ws.workspace.status, WorkspaceStatus::Dirty);
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&workspace.path).join("unsaved.txt")).unwrap(),
        "agent work"
    );
    // Recovery is explicit: resume starts a new process for the same native session.
    let resumed = client
        .call::<SessionsResume>(SessionsResumeParams { session_id: session.id, prompt: None })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.native_session_id, session.native_session_id);
    assert!(resumed.attached && resumed.pid != Some(agent_pid));

    client.call::<SystemShutdown>(ShutdownParams::default()).await.unwrap();
    assert!(wait_exit(&mut core).success());
    let resumed_pid = resumed.pid.unwrap();
    assert!(!pid_alive(resumed_pid), "graceful shutdown stops agents");
}
