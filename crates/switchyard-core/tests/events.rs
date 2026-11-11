#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Event journal and subscriptions through the IPC protocol.

mod common;

use switchyard_client::Notification;
use switchyard_protocol::methods::{
    EventsList, EventsListParams, EventsSubscribe, EventsSubscribeParams, EventsUnsubscribe,
    EventsUnsubscribeParams, ProjectIdParams, ProjectsAdd, ProjectsAddParams, ProjectsRemove,
};

use common::{TestCore, git_repo, next_notification};

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
