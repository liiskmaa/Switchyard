//! Accept loop and coordinated shutdown of client connections.

use std::time::Duration;

use tokio::net::UnixListener;
use tokio::task::JoinSet;

use crate::connection;
use crate::lifecycle::announce_stopping;
use crate::state::{CoreState, wait_shutdown};

pub(crate) async fn run(listener: UnixListener, owner_uid: u32, state: CoreState) {
    let mut shutdown = state.shutdown.subscribe();
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = wait_shutdown(&mut shutdown) => break,
            Some(finished) = connections.join_next(), if !connections.is_empty() => {
                if let Err(err) = finished {
                    tracing::error!(%err, "connection task failed");
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    match stream.peer_cred() {
                        Ok(cred) if cred.uid() == owner_uid => {
                            connections.spawn(connection::serve(stream, state.clone()));
                        }
                        Ok(cred) => tracing::warn!(uid = cred.uid(), "rejected connection from another user"),
                        Err(err) => tracing::warn!(%err, "rejected connection: cannot read peer credentials"),
                    }
                }
                Err(err) => {
                    // Typically EMFILE/ENFILE: back off instead of spinning.
                    tracing::error!(%err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
    drop(listener);

    let reason = state.shutdown.borrow().clone().unwrap_or_default();
    // Agents first, so their final events reach still-connected clients.
    state.agents.shutdown().await;
    announce_stopping(&state, &reason).await;
    state.closing.send_replace(true);

    // Connections now forward what subscribers already have (including
    // `core.stopping`), send a `shutdown` frame and close. Bounded wait.
    let grace = state.config.shutdown_grace;
    let drained = tokio::time::timeout(grace, async {
        while let Some(finished) = connections.join_next().await {
            if let Err(err) = finished {
                tracing::error!(%err, "connection task failed during shutdown");
            }
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!(remaining = connections.len(), "aborting connections that did not drain in time");
        connections.shutdown().await;
    }
}
