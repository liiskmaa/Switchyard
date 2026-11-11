//! Native agent sessions owned by Core.
//!
//! [`AgentService`] is the only place that starts, drives or stops agent
//! processes. The durable record of a session lives in SQLite; the in-memory
//! [`Attached`] entry exists only while this Core instance supervises the
//! session's process. Every state change goes through [`AgentService::commit`],
//! which validates the transition, writes it and its journal events in one
//! transaction, and then publishes the events.

mod approvals;
mod convert;
mod live;
mod supervisor;
mod watcher;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use switchyard_agents::claude::{self, ClaudeConfig, ClaudeInstallation, LaunchSpec, NativeSession};
use switchyard_agents::{AgentProvider, ApprovalStatus, DecisionSource, SessionState};
use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_git::Git;
use switchyard_protocol::methods::Delivery;
use switchyard_protocol::model::{AgentAuth, AgentAvailability, Session};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::sessions::{self, SessionChanges};
use switchyard_storage::{SessionRecord, Storage, StorageError, Transaction, projects};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub(crate) use live::LiveHub;
use watcher::WatcherHub;

use crate::config::CoreConfig;
use crate::handlers::errors;

/// Longest user message accepted (bytes).
const MAX_PROMPT_BYTES: usize = 100 * 1024;
/// How long a freshly spawned process must stay alive before the session
/// is reported `idle` (catches immediate start-up failures).
const LIVENESS_CHECK: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    Operator,
    CoreShutdown,
}

/// A session whose process this Core instance supervises.
pub(crate) struct Attached {
    handle: claude::ClaudeSession,
    project_id: Uuid,
    state: SessionState,
    /// Messages sent while a turn was running; each starts a turn later.
    queued_turns: u32,
    stopping: Option<StopReason>,
}

struct Detection {
    availability: AgentAvailability,
    installation: Option<ClaudeInstallation>,
}

#[derive(Clone)]
pub(crate) struct AgentService {
    inner: Arc<Inner>,
}

struct Inner {
    storage: Storage,
    bus: EventBus,
    config: Arc<CoreConfig>,
    live: LiveHub,
    watchers: WatcherHub,
    attached: Mutex<HashMap<Uuid, Attached>>,
    /// Serializes starts/resumes so duplicate-process checks are atomic.
    start_lock: AsyncMutex<()>,
    detection: AsyncMutex<Option<Detection>>,
}

pub(crate) fn draft(event: DomainEvent, ctx: &EventContext) -> Result<EventDraft, StorageError> {
    EventDraft::new(event, ctx.clone())
        .map_err(|e| StorageError::Corrupt(format!("cannot encode event: {e}")))
}

fn invalid_state(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidState, message)
}

impl AgentService {
    pub fn new(storage: Storage, bus: EventBus, git: Git, config: Arc<CoreConfig>) -> Self {
        let watchers = WatcherHub::new(bus.clone(), git, config.file_debounce);
        Self {
            inner: Arc::new(Inner {
                live: LiveHub::new(config.live_capacity),
                watchers,
                storage,
                bus,
                config,
                attached: Mutex::default(),
                start_lock: AsyncMutex::new(()),
                detection: AsyncMutex::new(None),
            }),
        }
    }

    pub fn live(&self) -> &LiveHub {
        &self.inner.live
    }

    fn attached(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Attached>> {
        // A poisoned map means a panic while holding it; continuing with the
        // inner value is safer than wedging every session.
        self.inner.attached.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_attached(&self, session_id: Uuid) -> bool {
        self.attached().contains_key(&session_id)
    }

    pub fn has_attached_for_project(&self, project_id: Uuid) -> Option<Uuid> {
        self.attached().iter().find(|(_, a)| a.project_id == project_id).map(|(id, _)| *id)
    }

    fn context(record: &SessionRecord) -> EventContext {
        EventContext {
            project_id: Some(record.project_id),
            workspace_id: record.workspace_id,
            session_id: Some(record.id),
            agent_id: Some(record.agent_id),
            ..Default::default()
        }
    }

    /// Runs `work` in one transaction, appends the drafts it returns, commits
    /// and publishes them in order.
    pub(crate) async fn commit<R, F>(&self, work: F) -> Result<R, ProtocolError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(R, Vec<EventDraft>), StorageError> + Send + 'static,
        R: Send + 'static,
    {
        let storage = self.inner.storage.clone();
        self.inner
            .bus
            .publish_batch_with(move || {
                storage.with_conn(|conn| {
                    let tx = conn.transaction()?;
                    let (value, drafts) = work(&tx)?;
                    let mut stored = Vec::with_capacity(drafts.len());
                    for d in drafts {
                        stored.push(switchyard_storage::events::append(&tx, d)?);
                    }
                    tx.commit()?;
                    Ok((value, stored))
                })
            })
            .await
            .map_err(errors::publish)?
            .map(|(value, _)| value)
            .map_err(errors::storage)
    }

    /// Validated state transition with `agent.state_changed` plus `extra`
    /// events, atomically. Updates the in-memory state on success.
    pub(crate) async fn transition(
        &self,
        session_id: Uuid,
        to: SessionState,
        changes: SessionChanges,
        extra: Vec<DomainEvent>,
    ) -> Result<SessionRecord, ProtocolError> {
        let now = now_ms();
        let record = self
            .commit(move |tx| {
                let before = sessions::get(tx, session_id)?;
                let after = sessions::transition(tx, session_id, to, &changes, now)?;
                let ctx = Self::context(&after);
                let mut drafts = Vec::new();
                if before.state != after.state {
                    drafts.push(draft(
                        DomainEvent::AgentStateChanged {
                            from: before.state.as_str().into(),
                            to: after.state.as_str().into(),
                        },
                        &ctx,
                    )?);
                }
                for e in extra {
                    drafts.push(draft(e, &ctx)?);
                }
                Ok((after, drafts))
            })
            .await?;
        if let Some(a) = self.attached().get_mut(&session_id) {
            a.state = record.state;
        }
        Ok(record)
    }

    /// Journals events for a session without changing its state.
    pub(crate) async fn record(
        &self,
        ctx: &EventContext,
        events: Vec<DraftOrEvent>,
    ) -> Result<(), ProtocolError> {
        let ctx = ctx.clone();
        let session_id = ctx.session_id;
        let now = now_ms();
        self.commit(move |tx| {
            if let Some(id) = session_id {
                sessions::touch(tx, id, now)?;
            }
            let drafts = events
                .into_iter()
                .map(|e| match e {
                    DraftOrEvent::Draft(d) => Ok(d),
                    DraftOrEvent::Event(e) => draft(e, &ctx),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(((), drafts))
        })
        .await
    }

    pub async fn session(&self, session_id: Uuid) -> Result<Session, ProtocolError> {
        let record =
            self.inner.storage.call(move |c| sessions::get(c, session_id)).await.map_err(errors::storage)?;
        self.to_wire(record).await
    }

    pub(crate) async fn to_wire(&self, record: SessionRecord) -> Result<Session, ProtocolError> {
        let id = record.id;
        let pending = self
            .inner
            .storage
            .call(move |c| switchyard_storage::approvals::count_pending(c, id))
            .await
            .map_err(errors::storage)?;
        Ok(convert::session(record, self.is_attached(id), pending))
    }

    pub async fn list(
        &self,
        project_id: Option<Uuid>,
        live_only: bool,
        limit: u32,
    ) -> Result<Vec<Session>, ProtocolError> {
        let records = self
            .inner
            .storage
            .call(move |c| sessions::list(c, project_id, live_only, limit))
            .await
            .map_err(errors::storage)?;
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            out.push(self.to_wire(r).await?);
        }
        Ok(out)
    }

    // --- detection ----------------------------------------------------------

    /// Claude Code availability. Cached; `refresh` re-runs detection.
    pub async fn availability(&self, refresh: bool) -> AgentAvailability {
        let mut cache = self.inner.detection.lock().await;
        if refresh || cache.is_none() {
            *cache = Some(self.detect().await);
        }
        cache.as_ref().map(|d| d.availability.clone()).unwrap_or_else(|| convert::unavailable("not detected"))
    }

    async fn detect(&self) -> Detection {
        let checked_at_ms = now_ms();
        match claude::detect(self.inner.config.claude_executable.as_deref()).await {
            Ok(installation) => {
                let auth = match claude::auth_status(&installation).await {
                    Ok(a) => {
                        Some(AgentAuth { logged_in: a.logged_in, method: a.method, provider: a.provider })
                    }
                    Err(err) => {
                        tracing::warn!(%err, "cannot read Claude Code auth status");
                        None
                    }
                };
                tracing::info!(version = %installation.version, path = %installation.executable.display(), "Claude Code detected");
                Detection {
                    availability: AgentAvailability {
                        provider: AgentProvider::ClaudeCode.as_str().into(),
                        display_name: "Claude Code".into(),
                        installed: true,
                        executable: Some(installation.executable.display().to_string()),
                        version: Some(installation.version.clone()),
                        tested_version: claude::TESTED_VERSION.into(),
                        error: None,
                        auth,
                        capabilities: claude::capabilities(),
                        checked_at_ms,
                    },
                    installation: Some(installation),
                }
            }
            Err(err) => {
                tracing::warn!(%err, "Claude Code is not usable");
                let mut availability = convert::unavailable(&err.to_string());
                availability.checked_at_ms = checked_at_ms;
                availability.installed = !matches!(err, claude::DetectError::NotFound);
                Detection { availability, installation: None }
            }
        }
    }

    /// A usable installation, re-detecting if the last attempt failed.
    async fn installation(&self) -> Result<ClaudeInstallation, ProtocolError> {
        let mut cache = self.inner.detection.lock().await;
        if cache.as_ref().is_none_or(|d| d.installation.is_none()) {
            *cache = Some(self.detect().await);
        }
        let detection =
            cache.as_ref().ok_or_else(|| ProtocolError::new(ErrorCode::AgentUnavailable, "not detected"))?;
        detection.installation.clone().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::AgentUnavailable,
                detection.availability.error.clone().unwrap_or_else(|| "Claude Code is not available".into()),
            )
        })
    }

    fn claude_config(&self, installation: &ClaudeInstallation) -> ClaudeConfig {
        let mut config = ClaudeConfig::new(installation.executable.clone());
        config.stop_grace = self.inner.config.agent_stop_grace;
        config.term_grace = self.inner.config.agent_term_grace;
        config
    }

    // --- lifecycle operations -------------------------------------------------

    async fn accessible_root(&self, project_id: Uuid) -> Result<PathBuf, ProtocolError> {
        let project =
            self.inner.storage.call(move |c| projects::get(c, project_id)).await.map_err(errors::storage)?;
        let root = PathBuf::from(&project.root_path);
        if !tokio::fs::metadata(&root).await.is_ok_and(|m| m.is_dir()) {
            return Err(ProtocolError::new(
                ErrorCode::RepositoryUnavailable,
                format!("{} is not accessible", project.root_path),
            ));
        }
        Ok(root)
    }

    fn ensure_project_free(&self, project_id: Uuid) -> Result<(), ProtocolError> {
        match self.has_attached_for_project(project_id) {
            Some(existing) => Err(ProtocolError::new(
                ErrorCode::Conflict,
                format!("project already has a running agent session ({existing}); stop it first"),
            )
            .with_data(serde_json::json!({ "session_id": existing }))),
            None => Ok(()),
        }
    }

    /// Starts a new Claude Code session in the project's registered checkout.
    pub async fn start(
        &self,
        project_id: Uuid,
        prompt: Option<String>,
        model: Option<String>,
    ) -> Result<Session, ProtocolError> {
        if let Some(p) = &prompt {
            validate_prompt(p)?;
        }
        let _guard = self.inner.start_lock.lock().await;
        let root = self.accessible_root(project_id).await?;
        self.ensure_project_free(project_id)?;
        let installation = self.installation().await?;

        let session_id = new_id();
        let native = new_id();
        let now = now_ms();
        let record = {
            let cwd = root.display().to_string();
            let executable = installation.executable.display().to_string();
            let version = installation.version.clone();
            let model = model.clone();
            self.commit(move |tx| {
                let agent_id = switchyard_storage::agents::upsert_agent(
                    tx,
                    AgentProvider::ClaudeCode,
                    "Claude Code",
                    &executable,
                    &version,
                    now,
                )?;
                let workspace_id = switchyard_storage::agents::primary_workspace(tx, project_id, &cwd, now)?;
                let record = SessionRecord {
                    id: session_id,
                    project_id,
                    workspace_id: Some(workspace_id),
                    agent_id,
                    provider: AgentProvider::ClaudeCode,
                    native_session_id: Some(native.to_string()),
                    cwd,
                    state: SessionState::Starting,
                    pid: None,
                    cli_version: Some(version),
                    model,
                    started_at_ms: now,
                    ended_at_ms: None,
                    last_activity_at_ms: now,
                    exit_code: None,
                    exit_signal: None,
                    failure: None,
                    resume_count: 0,
                };
                sessions::insert(tx, &record)?;
                Ok((record, Vec::new()))
            })
            .await?
        };
        let spec = LaunchSpec { session: NativeSession::New(native), cwd: root.clone(), model };
        self.launch(record, &installation, spec, false).await?;
        drop(_guard);
        if let Some(prompt) = prompt {
            self.send(session_id, prompt, false).await?;
        }
        self.session(session_id).await
    }

    /// Continues a finished session in a new process (`--resume`). The
    /// Switchyard session id and the native session id stay the same.
    pub async fn resume(&self, session_id: Uuid, prompt: Option<String>) -> Result<Session, ProtocolError> {
        if let Some(p) = &prompt {
            validate_prompt(p)?;
        }
        let _guard = self.inner.start_lock.lock().await;
        if self.is_attached(session_id) {
            return Err(ProtocolError::new(ErrorCode::Conflict, "session is already running"));
        }
        let record =
            self.inner.storage.call(move |c| sessions::get(c, session_id)).await.map_err(errors::storage)?;
        if !record.state.is_terminal() {
            return Err(invalid_state(format!(
                "session is {}; only finished sessions can be resumed",
                record.state
            )));
        }
        if record.provider != AgentProvider::ClaudeCode {
            return Err(ProtocolError::new(
                ErrorCode::Unsupported,
                "only Claude Code sessions can be resumed",
            ));
        }
        let native = record
            .native_session_id
            .clone()
            .ok_or_else(|| invalid_state("session has no native session id to resume"))?;
        let root = self.accessible_root(record.project_id).await?;
        self.ensure_project_free(record.project_id)?;
        let installation = self.installation().await?;

        let changes = SessionChanges {
            pid: Some(None),
            ended_at_ms: Some(None),
            exit_code: Some(None),
            exit_signal: Some(None),
            failure: Some(None),
            increment_resume_count: true,
            ..Default::default()
        };
        let record = self.transition(session_id, SessionState::Starting, changes, Vec::new()).await?;
        let spec =
            LaunchSpec { session: NativeSession::Resume(native), cwd: root, model: record.model.clone() };
        self.launch(record, &installation, spec, true).await?;
        drop(_guard);
        if let Some(prompt) = prompt {
            self.send(session_id, prompt, false).await?;
        }
        self.session(session_id).await
    }

    /// Spawns the process for a `starting` session and begins supervision.
    async fn launch(
        &self,
        record: SessionRecord,
        installation: &ClaudeInstallation,
        spec: LaunchSpec,
        resumed: bool,
    ) -> Result<(), ProtocolError> {
        let session_id = record.id;
        let (handle, updates) = match claude::spawn_session(&self.claude_config(installation), &spec) {
            Ok(pair) => pair,
            Err(err) => {
                let detail = err.to_string();
                tracing::error!(%session_id, %detail, "cannot start Claude Code");
                self.transition(
                    session_id,
                    SessionState::Failed,
                    SessionChanges {
                        ended_at_ms: Some(Some(now_ms())),
                        failure: Some(Some(format!("spawn_failed: {detail}"))),
                        ..Default::default()
                    },
                    vec![DomainEvent::AgentFailed {
                        reason: "spawn_failed".into(),
                        detail: Some(detail.clone()),
                    }],
                )
                .await?;
                return Err(ProtocolError::new(
                    ErrorCode::AgentFailed,
                    format!("cannot start Claude Code: {detail}"),
                )
                .with_data(serde_json::json!({ "session_id": session_id })));
            }
        };
        let pid = handle.pid();
        self.attached().insert(
            session_id,
            Attached {
                handle,
                project_id: record.project_id,
                state: SessionState::Starting,
                queued_turns: 0,
                stopping: None,
            },
        );
        let ctx = Self::context(&record);
        tokio::spawn(supervisor::run(self.clone(), session_id, ctx, updates));
        self.inner.watchers.attach(
            record.project_id,
            std::path::Path::new(&record.cwd),
            session_id,
            record.workspace_id,
        );

        let native = record.native_session_id.clone();
        let mut events = vec![DomainEvent::AgentStarted {
            provider: record.provider.as_str().into(),
            cli_version: installation.version.clone(),
            pid,
            cwd: record.cwd.clone(),
            native_session_id: native.clone(),
            resumed,
        }];
        if resumed {
            events.push(DomainEvent::AgentSessionResumed {
                native_session_id: native.unwrap_or_default(),
                resume_count: record.resume_count,
            });
        }
        self.transition(
            session_id,
            SessionState::Initializing,
            SessionChanges { pid: Some(Some(pid)), ..Default::default() },
            events,
        )
        .await?;

        // The CLI emits nothing until it receives input, so readiness is
        // "still alive shortly after launch". Early failures (bad flags,
        // missing auth) are reported by the exit path instead.
        tokio::time::sleep(LIVENESS_CHECK).await;
        if self.is_attached(session_id) {
            if let Err(err) =
                self.transition(session_id, SessionState::Idle, SessionChanges::default(), Vec::new()).await
            {
                tracing::debug!(%session_id, ?err, "session changed state during start-up");
            }
        } else {
            let failure = self.session(session_id).await.ok().and_then(|s| s.failure);
            return Err(ProtocolError::new(
                ErrorCode::AgentFailed,
                format!("Claude Code exited during start-up: {}", failure.unwrap_or_default()),
            )
            .with_data(serde_json::json!({ "session_id": session_id })));
        }
        Ok(())
    }

    /// Delivers a user message. See [`Delivery`] for the two outcomes.
    pub async fn send(
        &self,
        session_id: Uuid,
        text: String,
        queue_if_busy: bool,
    ) -> Result<Delivery, ProtocolError> {
        validate_prompt(&text)?;
        let (handle, state) = {
            let mut map = self.attached();
            let a = map.get_mut(&session_id).ok_or_else(|| {
                invalid_state("session has no running process; resume it to continue the conversation")
            })?;
            if a.stopping.is_some() {
                return Err(invalid_state("session is stopping"));
            }
            let state = a.state;
            if state.turn_in_progress() {
                if !queue_if_busy {
                    return Err(invalid_state(
                        "a turn is running; interrupt it, or send with queue_if_busy to run this message next",
                    ));
                }
                if !claude::capabilities().queue_message_during_turn {
                    return Err(ProtocolError::new(ErrorCode::Unsupported, "agent cannot queue messages"));
                }
                a.queued_turns += 1;
            } else if !state.accepts_new_turn() {
                return Err(invalid_state(format!("session is {state}; wait until it is idle")));
            }
            (a.handle.clone(), state)
        };
        let queued = state.turn_in_progress();
        let (prompt, prompt_truncated) = switchyard_agents::event::truncate(&text, 4096);
        let turn_event = DomainEvent::AgentTurnStarted { prompt, prompt_truncated, queued };
        if queued {
            let record = self
                .inner
                .storage
                .call(move |c| sessions::get(c, session_id))
                .await
                .map_err(errors::storage)?;
            self.record(&Self::context(&record), vec![DraftOrEvent::Event(turn_event)]).await?;
        } else {
            // Recorded before writing so the turn can never finish before it
            // is journaled as started.
            self.transition(session_id, SessionState::Running, SessionChanges::default(), vec![turn_event])
                .await?;
        }
        if let Err(err) = handle.send_message(text).await {
            let message = format!("message was not delivered: {err}");
            if let Ok(record) = self.inner.storage.call(move |c| sessions::get(c, session_id)).await {
                let _ = self
                    .record(
                        &Self::context(&record),
                        vec![DraftOrEvent::Event(DomainEvent::AgentWarning { message: message.clone() })],
                    )
                    .await;
            }
            return Err(ProtocolError::new(ErrorCode::AgentFailed, message));
        }
        Ok(if queued { Delivery::QueuedForNextTurn } else { Delivery::StartedTurn })
    }

    /// Asks the agent to end the running turn. The session becomes
    /// `interrupted` when the agent confirms the turn ended.
    pub async fn interrupt(&self, session_id: Uuid) -> Result<Session, ProtocolError> {
        let handle = {
            let map = self.attached();
            let a = map.get(&session_id).ok_or_else(|| invalid_state("session has no running process"))?;
            if !a.state.turn_in_progress() {
                return Err(invalid_state(format!("nothing to interrupt: session is {}", a.state)));
            }
            a.handle.clone()
        };
        handle.interrupt().await.map_err(|e| {
            ProtocolError::new(ErrorCode::AgentFailed, format!("interrupt not delivered: {e}"))
        })?;
        self.session(session_id).await
    }

    /// Gracefully stops the session's process: interrupt a running turn,
    /// close stdin, then escalate to SIGTERM/SIGKILL after grace periods.
    pub async fn stop(&self, session_id: Uuid) -> Result<Session, ProtocolError> {
        self.stop_with(session_id, StopReason::Operator).await?;
        self.session(session_id).await
    }

    async fn stop_with(&self, session_id: Uuid, reason: StopReason) -> Result<(), ProtocolError> {
        let (handle, in_turn) = {
            let mut map = self.attached();
            let a =
                map.get_mut(&session_id).ok_or_else(|| invalid_state("session has no running process"))?;
            a.stopping = Some(reason);
            (a.handle.clone(), a.state.turn_in_progress())
        };
        if in_turn && handle.interrupt().await.is_ok() {
            // Give the agent a moment to end the turn cleanly.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < deadline
                && self.attached().get(&session_id).is_some_and(|a| a.state.turn_in_progress())
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        if let Err(err) = handle.stop().await {
            tracing::debug!(%session_id, %err, "stop: process already gone");
        }
        // Wait for the supervisor to finalize the session record.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.is_attached(session_id) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(())
    }

    // --- Core lifecycle -------------------------------------------------------

    /// Marks sessions left live by a previous Core as failed. Their processes
    /// are not supervised by this instance (an orphaned Claude process exits
    /// when its stdin pipe closes); they can be resumed explicitly.
    pub async fn reconcile_on_startup(&self) -> Result<usize, ProtocolError> {
        let stale = self
            .inner
            .storage
            .call(|c| sessions::list(c, None, true, u32::MAX))
            .await
            .map_err(errors::storage)?;
        for record in &stale {
            let session_id = record.id;
            let ctx = Self::context(record);
            self.settle_approvals(session_id, &ctx, ApprovalStatus::Cancelled, DecisionSource::CoreRestart)
                .await?;
            self.transition(
                session_id,
                SessionState::Failed,
                SessionChanges {
                    pid: Some(None),
                    ended_at_ms: Some(Some(now_ms())),
                    failure: Some(Some(
                        "core_restarted: the previous Core instance ended while this session was live; \
                         its process is no longer supervised. Resume the session to continue."
                            .into(),
                    )),
                    ..Default::default()
                },
                vec![DomainEvent::AgentFailed { reason: "core_restarted".into(), detail: None }],
            )
            .await?;
        }
        if !stale.is_empty() {
            tracing::warn!(count = stale.len(), "marked sessions from a previous Core run as failed");
        }
        Ok(stale.len())
    }

    /// Stops every attached session (Core shutdown). Pending approvals are
    /// cancelled and the agents told so before their processes are stopped.
    pub async fn shutdown(&self) {
        let ids: Vec<Uuid> = self.attached().keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        tracing::info!(count = ids.len(), "stopping agent sessions for shutdown");
        let mut tasks = tokio::task::JoinSet::new();
        for id in ids {
            let svc = self.clone();
            tasks.spawn(async move {
                if let Some(a) = svc.attached().get_mut(&id) {
                    a.stopping = Some(StopReason::CoreShutdown);
                }
                if let Err(err) = svc.cancel_pending_for_shutdown(id).await {
                    tracing::error!(%id, ?err, "cannot settle approvals on shutdown");
                }
                if let Err(err) = svc.stop_with(id, StopReason::CoreShutdown).await {
                    tracing::debug!(%id, ?err, "session already stopped");
                }
            });
        }
        let budget =
            self.inner.config.agent_stop_grace + self.inner.config.agent_term_grace + Duration::from_secs(10);
        if tokio::time::timeout(budget, async { while tasks.join_next().await.is_some() {} }).await.is_err() {
            tracing::error!("agent sessions did not stop in time");
        }
    }
}

/// Either a prepared draft (to control its id for causation) or an event.
pub(crate) enum DraftOrEvent {
    Draft(EventDraft),
    Event(DomainEvent),
}

fn validate_prompt(text: &str) -> Result<(), ProtocolError> {
    if text.trim().is_empty() {
        return Err(ProtocolError::new(ErrorCode::InvalidParams, "message must not be empty"));
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParams,
            format!("message exceeds {MAX_PROMPT_BYTES} bytes"),
        ));
    }
    Ok(())
}
