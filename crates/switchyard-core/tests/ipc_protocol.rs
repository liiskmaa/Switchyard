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
