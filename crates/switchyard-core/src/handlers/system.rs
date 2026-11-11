use switchyard_protocol::PROTOCOL_VERSION;
use switchyard_protocol::ProtocolError;
use switchyard_protocol::methods::{ShutdownParams, ShutdownResult};
use switchyard_protocol::model::{DatabaseHealth, HealthReport, HealthStatus};
use switchyard_storage::stats;

use crate::state::CoreState;

/// Reports live health. Database figures come from a real query on every
/// call; a failing query yields `degraded`, never cached or invented values.
pub(crate) async fn health(state: &CoreState) -> Result<HealthReport, ProtocolError> {
    let db_path = state.storage.path().display().to_string();
    let (database, stats) = match state.storage.call(|c| stats::collect(c)).await {
        Ok(s) => (
            DatabaseHealth {
                initialized: s.journal_mode.eq_ignore_ascii_case("wal")
                    && s.schema_version == switchyard_storage::LATEST_SCHEMA_VERSION,
                path: db_path,
                journal_mode: Some(s.journal_mode.clone()),
                schema_version: Some(s.schema_version),
                error: None,
            },
            Some(s),
        ),
        Err(err) => {
            tracing::error!(%err, "health check database query failed");
            (
                DatabaseHealth {
                    initialized: false,
                    path: db_path,
                    journal_mode: None,
                    schema_version: None,
                    error: Some(err.to_string()),
                },
                None,
            )
        }
    };
    Ok(HealthReport {
        status: if database.initialized { HealthStatus::Ok } else { HealthStatus::Degraded },
        core: (*state.info).clone(),
        protocol_version: PROTOCOL_VERSION,
        uptime_ms: i64::try_from(state.started.elapsed().as_millis()).unwrap_or(i64::MAX),
        projects_count: stats.as_ref().map(|s| s.projects),
        active_sessions_count: stats.as_ref().map(|s| s.active_sessions),
        last_event_seq: stats.and_then(|s| s.last_event_seq),
        connected_clients: state.clients(),
        database,
    })
}

pub(crate) async fn shutdown(
    state: &CoreState,
    params: ShutdownParams,
) -> Result<ShutdownResult, ProtocolError> {
    state.request_shutdown(params.reason.unwrap_or_else(|| "requested by client".to_owned()));
    Ok(ShutdownResult { accepted: true })
}
