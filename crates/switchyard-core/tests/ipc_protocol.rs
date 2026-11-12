#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Wire-level protocol behaviour: handshake, version negotiation, errors.

mod common;

use serde_json::{Value, json};
use switchyard_client::ClientError;
use switchyard_protocol::methods::{Empty, SystemHealth};
use switchyard_protocol::model::HealthStatus;
use switchyard_protocol::{ErrorCode, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use common::TestCore;

struct Raw {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Raw {
    async fn connect(core: &TestCore) -> Self {
        let (r, w) = UnixStream::connect(core.socket()).await.unwrap().into_split();
        Self { reader: BufReader::new(r), writer: w }
    }

    async fn send(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.write_all(b"\n").await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn initialize(&mut self) -> Value {
        self.send(
            &json!({"type":"request","id":1,"method":"initialize","params":{
            "protocol_version": PROTOCOL_VERSION, "client": {"name":"raw","version":"1"}}})
            .to_string(),
        )
        .await;
        self.recv().await
    }
}

#[tokio::test]
async fn handshake_negotiates_version_and_reports_core_identity() {
    let core = TestCore::start().await;
    let mut raw = Raw::connect(&core).await;
    let reply = raw.initialize().await;
    assert_eq!(reply["type"], "response");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(reply["result"]["core"]["version"], switchyard_core::CORE_VERSION);
    assert_eq!(
        reply["result"]["core"]["instance_id"],
        core.core.as_ref().unwrap().info().instance_id.to_string()
    );
}

#[tokio::test]
async fn newer_client_is_downgraded_and_too_old_client_is_rejected() {
    let core = TestCore::start().await;
    let mut raw = Raw::connect(&core).await;
    raw.send(r#"{"type":"request","id":1,"method":"initialize","params":{"protocol_version":999,"client":{"name":"n","version":"1"}}}"#).await;
    assert_eq!(raw.recv().await["result"]["protocol_version"], PROTOCOL_VERSION);

    let mut raw = Raw::connect(&core).await;
    raw.send(r#"{"type":"request","id":1,"method":"initialize","params":{"protocol_version":0,"client":{"name":"o","version":"1"}}}"#).await;
    let reply = raw.recv().await;
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["error"]["code"], "unsupported_protocol_version");
    assert_eq!(reply["error"]["data"]["max_supported"], PROTOCOL_VERSION);
}

#[tokio::test]
async fn requests_before_initialize_are_rejected() {
    let core = TestCore::start().await;
    let mut raw = Raw::connect(&core).await;
    raw.send(r#"{"type":"request","id":5,"method":"system.health"}"#).await;
    let reply = raw.recv().await;
    assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (json!(5), json!("not_initialized")));
}

#[tokio::test]
async fn malformed_frames_get_errors_without_killing_the_connection() {
    let core = TestCore::start().await;
    let mut raw = Raw::connect(&core).await;
    raw.initialize().await;

    raw.send("this is not json").await;
    let reply = raw.recv().await;
    assert_eq!(reply["error"]["code"], "parse_error");
    assert!(reply["id"].is_null());

    raw.send(r#"{"type":"bogus","id":7}"#).await;
    let reply = raw.recv().await;
    assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (json!(7), json!("parse_error")));

    raw.send(r#"{"type":"request","id":8,"method":"no.such.method"}"#).await;
    assert_eq!(raw.recv().await["error"]["code"], "method_not_found");

    raw.send(r#"{"type":"request","id":9,"method":"projects.add","params":{"wrong":1}}"#).await;
    assert_eq!(raw.recv().await["error"]["code"], "invalid_params");

    raw.send(r#"{"type":"request","id":10,"method":"initialize","params":{}}"#).await;
    assert_eq!(raw.recv().await["error"]["code"], "already_initialized");

    raw.send(r#"{"type":"request","id":11,"method":"system.health"}"#).await;
    assert_eq!(raw.recv().await["type"], "response", "connection still usable");
}

#[tokio::test]
async fn oversized_frame_closes_the_connection() {
    let core = TestCore::start().await;
    let mut raw = Raw::connect(&core).await;
    raw.initialize().await;
    let huge = "x".repeat(switchyard_protocol::DEFAULT_MAX_FRAME_BYTES + 10);
    raw.send(&huge).await;
    assert_eq!(raw.recv().await["error"]["code"], "frame_too_large");
    let mut rest = String::new();
    let n = raw.reader.read_line(&mut rest).await.unwrap();
    assert_eq!(n, 0, "Core closes the connection after an oversized frame");
}

#[tokio::test]
async fn health_reports_live_database_state() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let health = client.call::<SystemHealth>(Empty {}).await.unwrap();
    assert_eq!(health.status, HealthStatus::Ok);
    assert!(health.database.initialized);
    assert_eq!(health.database.journal_mode.as_deref(), Some("wal"));
    assert_eq!(health.database.schema_version, Some(switchyard_storage::LATEST_SCHEMA_VERSION));
    assert_eq!(health.projects_count, Some(0));
    assert_eq!(health.active_sessions_count, Some(0));
    assert!(health.last_event_seq.is_some(), "core.started is journaled");
    assert_eq!(health.connected_clients, 1);
    assert_eq!(health.core.version, switchyard_core::CORE_VERSION);
}

#[tokio::test]
async fn remote_errors_surface_as_typed_client_errors() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let err = client
        .call::<switchyard_protocol::methods::ProjectsGet>(switchyard_protocol::methods::ProjectIdParams {
            project_id: uuid::Uuid::now_v7(),
        })
        .await
        .unwrap_err();
    match err {
        ClientError::Remote(e) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("unexpected {other:?}"),
    }
}

// --- protocol compatibility (ADR 0015) ---------------------------------------

/// Desktops from before workspaces (protocol 1), before policies
/// (protocol 2) and before Codex (protocol 3) connecting to this Core are refused during the handshake
/// with an actionable error, and the connection stays unusable: their next
/// request never reaches a handler.
#[tokio::test]
async fn old_client_is_refused_before_any_operation() {
    let core = TestCore::start().await;
    for (old, version) in [(1, "0.1.0-m2"), (2, "0.1.0-m3"), (3, "0.1.0-m4")] {
        let mut raw = Raw::connect(&core).await;
        raw.send(&format!(r#"{{"type":"request","id":1,"method":"initialize","params":{{"protocol_version":{old},"client":{{"name":"switchyard-desktop","version":"{version}"}}}}}}"#)).await;
        let reply = raw.recv().await;
        assert_eq!(reply["type"], "error");
        assert_eq!(reply["error"]["code"], "unsupported_protocol_version");
        let data = &reply["error"]["data"];
        assert_eq!(
            (data["min_supported"].clone(), data["max_supported"].clone()),
            (json!(4), json!(PROTOCOL_VERSION))
        );
        assert_eq!(data["client_protocol"], old);
        assert_eq!(data["client_version"], version);
        assert_eq!(data["core_version"], switchyard_core::CORE_VERSION);
        assert_eq!(data["action"], "update_client");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(message.contains(version) && message.contains("requires protocol 4"), "{message}");

        // The old client's next step fails safely instead of reaching Core.
        raw.send(r#"{"type":"request","id":2,"method":"approvals.decide","params":{"approval_id":"00000000-0000-0000-0000-000000000000","decision":"approve"}}"#).await;
        assert_eq!(raw.recv().await["error"]["code"], "not_initialized");
    }
}

/// This client connecting to an older Core (one that answers the handshake
/// with protocol 1, 2 or 3, as milestone-2 to milestone-4 Cores do) gives up
/// during the handshake with a typed, actionable error and sends nothing
/// else.
#[tokio::test]
async fn new_client_refuses_an_older_core() {
    for old in [1, 2, 3] {
        refuses_core_speaking(old).await;
    }
}

async fn refuses_core_speaking(old: u32) {
    let dir = common::data_dir();
    let socket = dir.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let old_core = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["protocol_version"], PROTOCOL_VERSION);
        // An old Core answers min(client, its own version).
        let reply = json!({"type": "response", "id": request["id"], "result": {"protocol_version": old, "core": {
            "version": "0.1.0-m2", "instance_id": "00000000-0000-0000-0000-000000000001",
            "started_at_ms": 1, "pid": 4242}}});
        w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        // Record anything the client sends afterwards.
        let mut rest = String::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_line(&mut rest)).await;
        rest
    });
    let err = match common::try_connect(&socket).await {
        Ok(_) => panic!("connected to an incompatible Core"),
        Err(e) => e,
    };
    match &err {
        ClientError::IncompatibleCore { core_protocol, required, core_version, core_pid, .. } => {
            assert_eq!(
                (*core_protocol, *required, core_version.as_str(), *core_pid),
                (old, 4, "0.1.0-m2", 4242)
            );
        }
        other => panic!("expected IncompatibleCore, got {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("Core 0.1.0-m2 (pid 4242)") && message.contains("Stop that Core"), "{message}");
    assert!(!err.is_connection_error(), "not retried as if Core were missing");
    assert_eq!(old_core.await.unwrap(), "", "no request after the failed handshake");
}

/// Matching versions still negotiate normally.
#[tokio::test]
async fn current_client_and_core_agree_on_protocol_4() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    assert_eq!(client.info().protocol_version, 4);
}
