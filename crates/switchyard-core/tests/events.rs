#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Event journal and subscriptions through the IPC protocol.

mod common;

use switchyard_client::Notification;
use switchyard_protocol::methods::{
    EventsList, EventsListParams, EventsSubscribe, EventsSubscribeParams, EventsUnsubscribe,
    EventsUnsubscribeParams, ProjectIdParams, ProjectsAdd, ProjectsAddParams, ProjectsRemove,
};

use common::{TestCore, git_repo, next_notification};
use switchyard_client::CoreClient;
use switchyard_protocol::EventEnvelope;
use tokio::sync::mpsc;

#[tokio::test]
async fn live_subscription_receives_persisted_events_in_order() {
    let core = TestCore::start().await;
    let (client, mut rx) = core.connect().await;
    let sub = client.call::<EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();

    let repo = git_repo(core.dir.path(), "eps");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    client.call::<ProjectsRemove>(ProjectIdParams { project_id: project.id }).await.unwrap();

    let mut received = Vec::new();
    for _ in 0..2 {
        match next_notification(&mut rx).await {
            Notification::Event { subscription_id, event } => {
                assert_eq!(subscription_id, sub.subscription_id);
                received.push(event);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(received[0].event_type, "project.registered");
    assert_eq!(received[1].event_type, "project.removed");
    assert!(received[1].seq > received[0].seq);
    assert_eq!(received[0].context.project_id, Some(project.id));
    assert_eq!(received[0].payload["root_path"], repo.display().to_string());

    // Everything delivered is in the journal.
    let stored = client
        .call::<EventsList>(EventsListParams { after_seq: Some(0), limit: Some(100), ..Default::default() })
        .await
        .unwrap()
        .events;
    for event in &received {
        assert!(stored.contains(event));
    }
}

#[tokio::test]
async fn subscription_replays_from_after_seq_then_continues_live() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "replay");
    client.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap();

    // A second client that "missed" everything after core.started.
    let (late, mut late_rx) = core.connect().await;
    let sub = late
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after_seq: Some(0),
            event_types: vec!["project.registered".into(), "project.removed".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    let replayed = match next_notification(&mut late_rx).await {
        Notification::Event { event, .. } => event,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(replayed.event_type, "project.registered");
    let project_id = replayed.context.project_id.unwrap();

    client.call::<ProjectsRemove>(ProjectIdParams { project_id }).await.unwrap();
    match next_notification(&mut late_rx).await {
        Notification::Event { event, .. } => {
            assert_eq!(event.event_type, "project.removed");
            assert!(event.seq > replayed.seq);
        }
        other => panic!("unexpected {other:?}"),
    }

    late.call::<EventsUnsubscribe>(EventsUnsubscribeParams { subscription_id: sub.subscription_id })
        .await
        .unwrap();
    let again = late
        .call::<EventsUnsubscribe>(EventsUnsubscribeParams { subscription_id: sub.subscription_id })
        .await;
    assert!(again.is_err());
}

#[tokio::test]
async fn events_list_returns_latest_page_and_validates_limits() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let latest = client.call::<EventsList>(EventsListParams::default()).await.unwrap().events;
    assert_eq!(latest.last().unwrap().event_type, "core.started");
    let bad = client.call::<EventsList>(EventsListParams { limit: Some(0), ..Default::default() }).await;
    assert!(bad.is_err());
}

#[tokio::test]
async fn subscribers_see_core_stopping_on_shutdown() {
    let mut core = TestCore::start().await;
    let (client, mut rx) = core.connect().await;
    client.call::<EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    core.core.as_ref().unwrap().shutdown("test shutdown");
    let mut saw_stopping = false;
    let mut saw_shutdown = false;
    while let Some(n) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap() {
        match n {
            Notification::Event { event, .. } if event.event_type == "core.stopping" => {
                assert_eq!(event.payload["reason"], "test shutdown");
                saw_stopping = true;
            }
            Notification::Shutdown { reason } => {
                assert_eq!(reason, "test shutdown");
                saw_shutdown = true;
            }
            _ => {}
        }
    }
    assert!(saw_stopping && saw_shutdown);
    client.closed().await;
    core.core.take().unwrap().wait().await.unwrap();
}

async fn recv_events(rx: &mut mpsc::Receiver<Notification>, n: usize) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    while out.len() < n {
        if let Notification::Event { event, .. } = next_notification(rx).await {
            out.push(event);
        }
    }
    out
}

async fn journal_after(client: &CoreClient, seq: i64) -> Vec<EventEnvelope> {
    client
        .call::<EventsList>(EventsListParams {
            after_seq: Some(seq),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
}

/// The desktop bridge's reconnect contract: remember the last delivered
/// `seq`, and after a lost connection subscribe with `after_seq` set to it.
/// Every durable event journaled while disconnected arrives exactly once, in
/// order, and live delivery continues after the replay without duplicates.
#[tokio::test]
async fn reconnecting_with_last_seen_seq_recovers_missed_events_exactly_once() {
    let core = TestCore::start().await;
    let (writer, _wrx) = core.connect().await;

    let (ui, mut ui_rx) = core.connect().await;
    ui.call::<EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let first = git_repo(core.dir.path(), "before");
    writer.call::<ProjectsAdd>(ProjectsAddParams { path: first.display().to_string() }).await.unwrap();
    let seen = recv_events(&mut ui_rx, 1).await;
    let last_seen = seen[0].seq;
    drop(ui);
    drop(ui_rx);

    // Durable events while the UI is disconnected.
    let mut missed_projects = Vec::new();
    for name in ["gap-a", "gap-b", "gap-c"] {
        let repo = git_repo(core.dir.path(), name);
        let p = writer
            .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
            .await
            .unwrap()
            .project;
        missed_projects.push(p.id);
    }
    writer.call::<ProjectsRemove>(ProjectIdParams { project_id: missed_projects[1] }).await.unwrap();
    let missed = journal_after(&writer, last_seen).await;
    assert_eq!(missed.len(), 4);

    let (ui, mut ui_rx) = core.connect().await;
    ui.call::<EventsSubscribe>(EventsSubscribeParams { after_seq: Some(last_seen), ..Default::default() })
        .await
        .unwrap();
    let replayed = recv_events(&mut ui_rx, missed.len()).await;
    assert_eq!(replayed, missed, "exactly the missed events, in journal order");

    // Live delivery continues with the next event only (no duplicate replay).
    let after = git_repo(core.dir.path(), "after");
    writer.call::<ProjectsAdd>(ProjectsAddParams { path: after.display().to_string() }).await.unwrap();
    let live = recv_events(&mut ui_rx, 1).await;
    assert_eq!(live[0].event_type, "project.registered");
    assert_eq!(live[0].seq, journal_after(&writer, missed.last().unwrap().seq).await[0].seq);
    let mut seqs: Vec<_> = replayed.iter().chain(&live).map(|e| e.seq).collect();
    let n = seqs.len();
    seqs.dedup();
    assert_eq!(seqs.len(), n, "no event delivered twice");
    assert!(tokio::time::timeout(std::time::Duration::from_millis(300), ui_rx.recv()).await.is_err());
}

/// The same contract across a Core restart: the journal survives, so a
/// client that remembers its last `seq` recovers what the previous Core
/// journaled after it (including `core.stopping`) and the new `core.started`.
#[tokio::test]
async fn replay_after_seq_spans_a_core_restart() {
    let mut core = TestCore::start().await;
    let (client, mut rx) = core.connect().await;
    client.call::<EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let repo = git_repo(core.dir.path(), "restart");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let last_seen = recv_events(&mut rx, 1).await[0].seq;
    drop(client);
    drop(rx);
    core.restart().await;

    let (writer, _wrx) = core.connect().await;
    writer.call::<ProjectsRemove>(ProjectIdParams { project_id: project.id }).await.unwrap();
    let missed = journal_after(&writer, last_seen).await;
    let types: Vec<_> = missed.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        types.contains(&"core.stopping")
            && types.contains(&"core.started")
            && types.contains(&"project.removed"),
        "{types:?}"
    );

    let (ui, mut ui_rx) = core.connect().await;
    ui.call::<EventsSubscribe>(EventsSubscribeParams { after_seq: Some(last_seen), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(recv_events(&mut ui_rx, missed.len()).await, missed);
}
