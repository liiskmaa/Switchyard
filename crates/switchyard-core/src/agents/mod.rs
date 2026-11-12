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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use switchyard_agents::claude::{self, ClaudeConfig, ClaudeInstallation, NativeSession};
use switchyard_agents::codex::{self, CodexConfig, CodexInstallation};
use switchyard_agents::{
    AgentProvider, AgentSession, ApprovalStatus, DecisionSource, SessionState, SessionUpdate,
};
use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_git::Git;
use switchyard_protocol::methods::Delivery;
use switchyard_protocol::model::{AgentAuth, AgentAvailability, Session};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::sessions::{self, SessionChanges, SessionFilter};
use switchyard_storage::{SessionRecord, Storage, StorageError, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use uuid::Uuid;

pub(crate) use live::LiveHub;
use watcher::WatcherHub;

use crate::config::CoreConfig;
use crate::handlers::errors;
use crate::workspaces::{self, WorkspaceService};

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
    handle: AgentSession,
    provider: AgentProvider,
    /// The adapter reported its native identity (`agent.ready`).
    ready: bool,
    project_id: Uuid,
    workspace_id: Option<Uuid>,
    state: SessionState,
    /// Messages sent while a turn was running; each starts a turn later.
    queued_turns: u32,
    stopping: Option<StopReason>,
}

/// A usable CLI installation, per provider.
#[derive(Debug, Clone)]
enum Installation {
    Claude(ClaudeInstallation),
    Codex(CodexInstallation),
}

impl Installation {
    fn executable(&self) -> &std::path::Path {
        match self {
            Self::Claude(i) => &i.executable,
            Self::Codex(i) => &i.executable,
        }
    }
    fn version(&self) -> &str {
        match self {
            Self::Claude(i) => &i.version,
            Self::Codex(i) => &i.version,
        }
    }
}

struct Detection {
    availability: AgentAvailability,
    installation: Option<Installation>,
}

/// What to launch, per provider.
enum Launch {
    Claude(NativeSession),
    Codex { resume_thread: Option<String> },
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
    workspaces: WorkspaceService,
    attached: Mutex<HashMap<Uuid, Attached>>,
    /// Serializes starts/resumes so duplicate-process checks are atomic.
    start_lock: AsyncMutex<()>,
    detection: AsyncMutex<HashMap<AgentProvider, Detection>>,
}

pub(crate) use crate::journal::draft;

fn invalid_state(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidState, message)
}

impl AgentService {
    pub fn new(
        storage: Storage,
        bus: EventBus,
        git: Git,
        workspaces: WorkspaceService,
        config: Arc<CoreConfig>,
    ) -> Self {
        let watchers = WatcherHub::new(bus.clone(), git, config.file_debounce, workspaces.clone());
        Self {
            inner: Arc::new(Inner {
                live: LiveHub::new(config.live_capacity),
                watchers,
                workspaces,
                storage,
                bus,
                config,
                attached: Mutex::default(),
                start_lock: AsyncMutex::new(()),
                detection: AsyncMutex::new(HashMap::new()),
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

    pub(crate) async fn commit<R, F>(&self, work: F) -> Result<R, ProtocolError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(R, Vec<EventDraft>), StorageError> + Send + 'static,
        R: Send + 'static,
    {
        crate::journal::commit(&self.inner.storage, &self.inner.bus, work).await
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

    /// Applies `changes` and journals `extra` without changing the state. The
    /// state is read inside the transaction, so this never races a concurrent
    /// transition (e.g. `agent.ready` arriving while start-up advances).
    pub(crate) async fn update_in_place(
        &self,
        session_id: Uuid,
        changes: SessionChanges,
        extra: Vec<DomainEvent>,
    ) -> Result<SessionRecord, ProtocolError> {
        let now = now_ms();
        self.commit(move |tx| {
            let before = sessions::get(tx, session_id)?;
            let after = sessions::transition(tx, session_id, before.state, &changes, now)?;
            let ctx = Self::context(&after);
            let drafts = extra.into_iter().map(|e| draft(e, &ctx)).collect::<Result<Vec<_>, _>>()?;
            Ok((after, drafts))
        })
        .await
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
        let (pending, workspace) = self
            .inner
            .storage
            .call(move |c| {
                Ok((
                    switchyard_storage::approvals::count_pending(c, id)?,
                    switchyard_storage::workspaces::for_session(c, id)?,
                ))
            })
            .await
            .map_err(errors::storage)?;
        Ok(convert::session(record, self.is_attached(id), pending, workspace.map(|(_, name)| name)))
    }

    pub async fn list(&self, filter: SessionFilter, limit: u32) -> Result<Vec<Session>, ProtocolError> {
        let records = self
            .inner
            .storage
            .call(move |c| sessions::list(c, filter, limit))
            .await
            .map_err(errors::storage)?;
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            out.push(self.to_wire(r).await?);
        }
        Ok(out)
    }

    // --- detection ----------------------------------------------------------

    /// Availability of every supported agent. Cached; `refresh` re-runs detection.
    pub async fn availability_all(&self, refresh: bool) -> Vec<AgentAvailability> {
        let mut out = Vec::new();
        for provider in AgentProvider::ALL {
            out.push(self.availability(*provider, refresh).await);
        }
        out
    }

    pub async fn availability(&self, provider: AgentProvider, refresh: bool) -> AgentAvailability {
        let mut cache = self.inner.detection.lock().await;
        if refresh || !cache.contains_key(&provider) {
            let d = self.detect(provider).await;
            cache.insert(provider, d);
        }
        cache
            .get(&provider)
            .map(|d| d.availability.clone())
            .unwrap_or_else(|| convert::unavailable(provider, "not detected"))
    }

    async fn detect(&self, provider: AgentProvider) -> Detection {
        let checked_at_ms = now_ms();
        let found: Result<(Installation, Option<AgentAuth>), (bool, String)> = match provider {
            AgentProvider::ClaudeCode => match claude::detect(self.inner.config.claude_executable.as_deref())
                .await
            {
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
                    Ok((Installation::Claude(installation), auth))
                }
                Err(err) => Err((!matches!(err, claude::DetectError::NotFound), err.to_string())),
            },
            AgentProvider::Codex => {
                match codex::detect(self.inner.config.codex_executable.as_deref()).await {
                    Ok(installation) => {
                        let auth = match codex::auth_status(&installation).await {
                            Ok(a) => {
                                Some(AgentAuth { logged_in: a.logged_in, method: a.method, provider: None })
                            }
                            Err(err) => {
                                tracing::warn!(%err, "cannot read Codex auth status");
                                None
                            }
                        };
                        Ok((Installation::Codex(installation), auth))
                    }
                    Err(err) => Err((!matches!(err, codex::DetectError::NotFound), err.to_string())),
                }
            }
        };
        // Codex's app-server protocol is experimental and changes per release:
        // an untested version is not trusted like the verified one.
        let found = match found {
            Ok((installation, _))
                if provider == AgentProvider::Codex
                    && installation.version() != codex::TESTED_VERSION
                    && !self.inner.config.allow_untested_codex =>
            {
                Err((
                    true,
                    format!(
                        "Codex {} is untested: Switchyard was verified with {} only, and the app-server \
                         protocol changes between releases. Install {} or set \
                         SWITCHYARD_ALLOW_UNTESTED_CODEX=1 to use it anyway.",
                        installation.version(),
                        codex::TESTED_VERSION,
                        codex::TESTED_VERSION
                    ),
                    Some(installation.version().to_owned()),
                    Some(installation.executable().display().to_string()),
                ))
            }
            Ok(ok) => Ok(ok),
            Err((installed, error)) => Err((installed, error, None, None)),
        };
        match found {
            Ok((installation, auth)) => {
                tracing::info!(%provider, version = %installation.version(), path = %installation.executable().display(), "agent CLI detected");
                Detection {
                    availability: AgentAvailability {
                        provider: provider.as_str().into(),
                        display_name: switchyard_agents::display_name(provider).into(),
                        installed: true,
                        executable: Some(installation.executable().display().to_string()),
                        version: Some(installation.version().to_owned()),
                        tested_version: switchyard_agents::tested_version(provider).into(),
                        error: None,
                        auth,
                        capabilities: switchyard_agents::capabilities(provider),
                        checked_at_ms,
                    },
                    installation: Some(installation),
                }
            }
            Err((installed, error, version, executable)) => {
                tracing::warn!(%provider, %error, "agent CLI is not usable");
                let mut availability = convert::unavailable(provider, &error);
                availability.checked_at_ms = checked_at_ms;
                availability.installed = installed;
                availability.version = version;
                availability.executable = executable;
                Detection { availability, installation: None }
            }
        }
    }

    /// A usable installation, re-detecting if the last attempt failed.
    async fn installation(&self, provider: AgentProvider) -> Result<Installation, ProtocolError> {
        let mut cache = self.inner.detection.lock().await;
        if cache.get(&provider).is_none_or(|d| d.installation.is_none()) {
            let d = self.detect(provider).await;
            cache.insert(provider, d);
        }
        let detection = cache
            .get(&provider)
            .ok_or_else(|| ProtocolError::new(ErrorCode::AgentUnavailable, "not detected"))?;
        detection.installation.clone().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::AgentUnavailable,
                detection.availability.error.clone().unwrap_or_else(|| {
                    format!("{} is not available", switchyard_agents::display_name(provider))
                }),
            )
        })
    }

    fn spawn(
        &self,
        installation: &Installation,
        launch: Launch,
        cwd: std::path::PathBuf,
        model: Option<String>,
    ) -> Result<(AgentSession, mpsc::Receiver<SessionUpdate>), switchyard_agents::SessionError> {
        let stop_grace = self.inner.config.agent_stop_grace;
        let term_grace = self.inner.config.agent_term_grace;
        match (installation, launch) {
            (Installation::Claude(i), Launch::Claude(session)) => {
                let mut config = ClaudeConfig::new(i.executable.clone());
                config.stop_grace = stop_grace;
                config.term_grace = term_grace;
                claude::spawn_session(&config, &claude::LaunchSpec { session, cwd, model })
            }
            (Installation::Codex(i), Launch::Codex { resume_thread }) => {
                let mut config = CodexConfig::new(i.executable.clone());
                config.stop_grace = stop_grace;
                config.term_grace = term_grace;
                codex::spawn_session(&config, &codex::LaunchSpec { cwd, model, resume_thread })
            }
            _ => Err(switchyard_agents::SessionError::Write("provider and launch mismatch".into())),
        }
    }

    // --- lifecycle operations -------------------------------------------------

    /// Starts a new agent session in a workspace. The workspace must
    /// belong to `project_id` (when given), accept a session and validate on
    /// disk and in Git; the agent's working directory is exactly its path.
    /// There is no fallback to the project checkout.
    pub async fn start(
        &self,
        provider: AgentProvider,
        workspace_id: Uuid,
        project_id: Option<Uuid>,
        prompt: Option<String>,
        model: Option<String>,
    ) -> Result<Session, ProtocolError> {
        if let Some(p) = &prompt {
            validate_prompt(p)?;
        }
        let _guard = self.inner.start_lock.lock().await;
        let workspace = self.inner.workspaces.validate_for_session(workspace_id, project_id).await?;
        let installation = self.installation(provider).await?;

        let session_id = new_id();
        // Claude takes a caller-chosen id; Codex assigns its thread id.
        let native = match provider {
            AgentProvider::ClaudeCode => Some(new_id()),
            AgentProvider::Codex => None,
        };
        let now = now_ms();
        let cwd = workspace.path.clone();
        let record = {
            let cwd = cwd.clone();
            let executable = installation.executable().display().to_string();
            let version = installation.version().to_owned();
            let model = model.clone();
            let project_id = workspace.project_id;
            crate::journal::commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
                let ws_drafts = match workspaces::attach_in_tx(tx, workspace_id, now)? {
                    Ok(d) => d,
                    Err(e) => return Ok(Err(e)),
                };
                let agent_id = switchyard_storage::agents::upsert_agent(
                    tx,
                    provider,
                    switchyard_agents::display_name(provider),
                    &executable,
                    &version,
                    now,
                )?;
                let record = SessionRecord {
                    id: session_id,
                    project_id,
                    workspace_id: Some(workspace_id),
                    agent_id,
                    provider,
                    native_session_id: native.map(|n| n.to_string()),
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
                Ok(Ok((record, ws_drafts)))
            })
            .await?
        };
        let launch = match native {
            Some(id) => Launch::Claude(NativeSession::New(id)),
            None => Launch::Codex { resume_thread: None },
        };
        self.launch(record, &installation, launch, cwd.into(), model, false).await?;
        drop(_guard);
        if let Some(prompt) = prompt {
            self.send(session_id, prompt, false).await?;
        }
        self.session(session_id).await
    }

    /// Continues a finished session in a new process (`--resume`), in the
    /// same workspace. The Switchyard session id and the native session id
    /// stay the same. Sessions from before workspace isolation (which ran in
    /// the project checkout) are not resumed.
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
        if !switchyard_agents::capabilities(record.provider).resume_session.is_supported() {
            return Err(ProtocolError::new(
                ErrorCode::Unsupported,
                format!("{} sessions cannot be resumed", switchyard_agents::display_name(record.provider)),
            ));
        }
        let native = record
            .native_session_id
            .clone()
            .ok_or_else(|| invalid_state("session has no native session id to resume"))?;
        let workspace_id = record.workspace_id.ok_or_else(|| {
            ProtocolError::new(ErrorCode::LegacyWorkspace, "session has no workspace; start a new session")
        })?;
        let workspace = self
            .inner
            .workspaces
            .validate_for_session(workspace_id, Some(record.project_id))
            .await
            .map_err(|e| {
                if e.code == ErrorCode::LegacyWorkspace {
                    ProtocolError::new(
                        ErrorCode::LegacyWorkspace,
                        "this session ran in the project checkout before workspace isolation; Switchyard \
                             will not resume it there. Start a new session in a workspace.",
                    )
                    .with_data(serde_json::json!({ "session_id": session_id, "workspace_id": workspace_id }))
                } else {
                    e
                }
            })?;
        if workspace.path != record.cwd {
            return Err(ProtocolError::new(
                ErrorCode::WorktreeIdentityMismatch,
                "the session's recorded working directory differs from its workspace path",
            ));
        }
        let installation = self.installation(record.provider).await?;

        let changes = SessionChanges {
            pid: Some(None),
            ended_at_ms: Some(None),
            exit_code: Some(None),
            exit_signal: Some(None),
            failure: Some(None),
            increment_resume_count: true,
            ..Default::default()
        };
        let now = now_ms();
        let record = crate::journal::commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let mut drafts = match workspaces::attach_in_tx(tx, workspace_id, now)? {
                Ok(d) => d,
                Err(e) => return Ok(Err(e)),
            };
            let before = sessions::get(tx, session_id)?;
            let after = sessions::transition(tx, session_id, SessionState::Starting, &changes, now)?;
            drafts.push(draft(
                DomainEvent::AgentStateChanged {
                    from: before.state.as_str().into(),
                    to: after.state.as_str().into(),
                },
                &Self::context(&after),
            )?);
            Ok(Ok((after, drafts)))
        })
        .await?;
        let launch = match record.provider {
            AgentProvider::ClaudeCode => Launch::Claude(NativeSession::Resume(native)),
            AgentProvider::Codex => Launch::Codex { resume_thread: Some(native) },
        };
        let model = record.model.clone();
        self.launch(record, &installation, launch, workspace.path.into(), model, true).await?;
        drop(_guard);
        if let Some(prompt) = prompt {
            self.send(session_id, prompt, false).await?;
        }
        self.session(session_id).await
    }

    /// Spawns the process for a `starting` session and begins supervision.
    #[allow(clippy::too_many_arguments)]
    async fn launch(
        &self,
        record: SessionRecord,
        installation: &Installation,
        launch: Launch,
        cwd: std::path::PathBuf,
        model: Option<String>,
        resumed: bool,
    ) -> Result<(), ProtocolError> {
        let session_id = record.id;
        let name = switchyard_agents::display_name(record.provider);
        let (handle, updates) = match self.spawn(installation, launch, cwd, model) {
            Ok(pair) => pair,
            Err(err) => {
                let detail = err.to_string();
                tracing::error!(%session_id, %detail, provider = %record.provider, "cannot start agent");
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
                if let Some(ws) = record.workspace_id {
                    self.inner.workspaces.release(ws).await;
                }
                return Err(ProtocolError::new(
                    ErrorCode::AgentFailed,
                    format!("cannot start {name}: {detail}"),
                )
                .with_data(serde_json::json!({ "session_id": session_id })));
            }
        };
        let pid = handle.pid();
        self.attached().insert(
            session_id,
            Attached {
                handle,
                provider: record.provider,
                ready: false,
                project_id: record.project_id,
                workspace_id: record.workspace_id,
                state: SessionState::Starting,
                queued_turns: 0,
                stopping: None,
            },
        );
        let ctx = Self::context(&record);
        tokio::spawn(supervisor::run(self.clone(), session_id, ctx, updates));
        if let Some(workspace_id) = record.workspace_id {
            self.inner.watchers.attach(
                workspace_id,
                std::path::Path::new(&record.cwd),
                EventContext {
                    project_id: Some(record.project_id),
                    workspace_id: Some(workspace_id),
                    session_id: Some(session_id),
                    ..Default::default()
                },
            );
        }

        let native = record.native_session_id.clone();
        let untested = installation.version() != switchyard_agents::tested_version(record.provider);
        let mut events = vec![DomainEvent::AgentStarted {
            provider: record.provider.as_str().into(),
            cli_version: installation.version().to_owned(),
            pid,
            cwd: record.cwd.clone(),
            native_session_id: native.clone(),
            resumed,
        }];
        if untested && record.provider == AgentProvider::Codex {
            events.push(DomainEvent::AgentWarning {
                message: format!(
                    "running untested Codex {} (verified with {}); allowed by SWITCHYARD_ALLOW_UNTESTED_CODEX",
                    installation.version(),
                    codex::TESTED_VERSION
                ),
            });
        }
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

        // Claude emits nothing until it receives input, so its readiness is
        // "still alive shortly after launch". Codex completes a handshake and
        // opens (or resumes) its thread first; the session is only reported
        // idle once that succeeded, so a failed resume is never presented as
        // a resumed conversation. Early failures are reported by the exit path.
        tokio::time::sleep(LIVENESS_CHECK).await;
        if record.provider == AgentProvider::Codex {
            let deadline = tokio::time::Instant::now() + self.inner.config.agent_ready_timeout;
            loop {
                match self.attached().get(&session_id).map(|a| a.ready) {
                    Some(true) | None => break,
                    Some(false) => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::error!(%session_id, "Codex did not open its thread in time; stopping");
                    let _ = self.stop_with(session_id, StopReason::Operator).await;
                    return Err(ProtocolError::new(
                        ErrorCode::AgentFailed,
                        format!("{name} did not become ready in time"),
                    )
                    .with_data(serde_json::json!({ "session_id": session_id })));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
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
                format!("{name} exited during start-up: {}", failure.unwrap_or_default()),
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
                if !switchyard_agents::capabilities(a.provider).queue_message_during_turn.is_supported() {
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
        let session = self.session(session_id).await?;
        // Settle the workspace before returning (idempotent with the
        // supervisor's own release), so it is usable right away.
        if let Some(ws) = session.workspace_id {
            self.inner.workspaces.release(ws).await;
        }
        Ok(session)
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
            .call(|c| sessions::list(c, SessionFilter { live_only: true, ..Default::default() }, u32::MAX))
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
