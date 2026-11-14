//! Task orchestration (Milestone 6, ADRs 0023-0027).
//!
//! A task is durable work, independent of any session. An *assignment* gives
//! one role on a task (investigate, implement, review) to one agent provider
//! in one workspace; Core starts or resumes a native session for it and sends
//! it the versioned task context. The agent answers with a `switchyard-report`
//! block, which Core records as a structured message: a claim or a request,
//! never an authorization. Every decision (publish, assign, complete, fail,
//! approve a proposed subtask) is the operator's.
//!
//! Every change goes through [`crate::journal::commit_checked`]: the task
//! rows and their `task.*` events are written in one transaction.

mod assign;
mod convert;
mod reactor;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_protocol::methods::{
    TaskDetail, TasksCreateParams, TasksDecideProposalResult, TasksUpdateParams,
};
use switchyard_protocol::model::{Task, TaskReadiness};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::tasks::{self as store, MessageRecord, StatusChange, TaskEdit, TaskRecord};
use switchyard_storage::{Connection, Storage, StorageError, projects};
use switchyard_tasks::{
    MessageKind, Priority, ProposalStatus, ProposedSubtask, Readiness, SubtaskDependency, TaskKind,
    TaskStatus, readiness, would_create_cycle,
};
use uuid::Uuid;

use crate::agents::AgentService;
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};
use crate::workspaces::WorkspaceService;

pub(crate) use reactor::spawn_reactor;

/// Status reasons Core sets for dependency blocking start with this, so a
/// blocker reported by an agent is never lifted automatically.
const DEPENDENCY_REASON: &str = "dependencies:";
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 20_000;
const MAX_CRITERIA: usize = 50;
const MAX_CRITERION: usize = 1000;
const MAX_NOTE: usize = 20_000;

/// Inner result of a transaction: a domain refusal (nothing written) or a value.
type Checked<T> = Result<Result<T, ProtocolError>, StorageError>;

/// Propagates a domain refusal out of a `commit_checked` closure.
macro_rules! check {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        }
    };
}
pub(crate) use check;

pub(crate) fn invalid_state(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidState, message)
}

pub(crate) fn invalid_params(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, message)
}

fn not_found(err: StorageError, code: ErrorCode) -> StorageErrorOr {
    match err {
        StorageError::NotFound(what) => {
            StorageErrorOr::Domain(ProtocolError::new(code, format!("{what} not found")))
        }
        other => StorageErrorOr::Storage(other),
    }
}

enum StorageErrorOr {
    Domain(ProtocolError),
    Storage(StorageError),
}

/// Looks up a row inside a transaction, turning "not found" into a domain error.
fn lookup<T>(r: Result<T, StorageError>, code: ErrorCode) -> Checked<T> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(e) => match not_found(e, code) {
            StorageErrorOr::Domain(p) => Ok(Err(p)),
            StorageErrorOr::Storage(s) => Err(s),
        },
    }
}

pub(crate) fn task_context(t: &TaskRecord) -> EventContext {
    EventContext { project_id: Some(t.project_id), task_id: Some(t.id), ..Default::default() }
}

fn bounded(text: &str, max: usize) -> String {
    switchyard_agents::event::truncate(text, max).0
}

/// Moves a task to `to`, validated against the state machine, journals it and
/// re-evaluates the readiness of its dependents.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_status(
    tx: &Connection,
    task_id: Uuid,
    to: TaskStatus,
    reason: Option<String>,
    result: Option<&Value>,
    failure: Option<&str>,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<TaskRecord> {
    let current = store::get_task(tx, task_id)?;
    if let Err(e) = current.status.transition(to) {
        return Ok(Err(invalid_state(format!("task is {}; {e}", current.status))));
    }
    let (before, after) = store::set_task_status(
        tx,
        task_id,
        to,
        StatusChange { reason: reason.as_deref(), result, failure },
        now,
    )?;
    drafts.push(draft(
        DomainEvent::TaskStatusChanged {
            from: before.status.as_str().into(),
            to: after.status.as_str().into(),
            reason,
        },
        &task_context(&after),
    )?);
    reevaluate_dependents(tx, task_id, now, drafts)?;
    Ok(Ok(after))
}

/// `ready`/`blocked` (with reason) from a task's dependencies.
fn readiness_status(tx: &Connection, task_id: Uuid) -> Result<(TaskStatus, Option<String>), StorageError> {
    Ok(match readiness(&store::dependencies_of(tx, task_id)?) {
        Readiness::Satisfied => (TaskStatus::Ready, None),
        Readiness::Waiting(w) => {
            (TaskStatus::Blocked, Some(format!("{DEPENDENCY_REASON} waiting for {} task(s)", w.len())))
        }
        Readiness::Broken(b) => (
            TaskStatus::Blocked,
            Some(format!("{DEPENDENCY_REASON} {} task(s) failed or were cancelled", b.len())),
        ),
    })
}

/// Moves `ready`/`blocked` dependents of `task_id` to match their
/// dependencies. Drafts and tasks being worked on are left alone; an agent's
/// blocker is never lifted here.
fn reevaluate_dependents(
    tx: &Connection,
    task_id: Uuid,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Result<(), StorageError> {
    for dependent in store::dependents_of(tx, task_id)? {
        let t = store::get_task(tx, dependent)?;
        let (want, reason) = readiness_status(tx, dependent)?;
        let blocked_by_dependencies =
            t.status_reason.as_deref().is_some_and(|r| r.starts_with(DEPENDENCY_REASON));
        let change = match (t.status, want) {
            (TaskStatus::Blocked, TaskStatus::Ready) if blocked_by_dependencies => true,
            (TaskStatus::Ready, TaskStatus::Blocked) => true,
            _ => false,
        };
        if change {
            let (before, after) = store::set_task_status(
                tx,
                dependent,
                want,
                StatusChange { reason: reason.as_deref(), result: None, failure: None },
                now,
            )?;
            drafts.push(draft(
                DomainEvent::TaskStatusChanged {
                    from: before.status.as_str().into(),
                    to: after.status.as_str().into(),
                    reason,
                },
                &task_context(&after),
            )?);
        }
    }
    Ok(())
}

/// A new message about `task`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_message(
    task: &TaskRecord,
    assignment_id: Option<Uuid>,
    kind: MessageKind,
    sender: &str,
    sender_session_id: Option<Uuid>,
    recipient: &str,
    payload: Value,
    causation_id: Option<String>,
    delivery: &str,
    now: i64,
) -> MessageRecord {
    MessageRecord {
        id: new_id(),
        project_id: task.project_id,
        task_id: task.id,
        assignment_id,
        kind,
        sender: sender.into(),
        sender_session_id,
        recipient: recipient.into(),
        recipient_assignment_id: None,
        payload,
        causation_id,
        correlation_id: task.id.to_string(),
        delivery: delivery.into(),
        delivered_at_ms: None,
        created_at_ms: now,
    }
}

/// Inserts a message and journals it. `false` when it was already recorded.
pub(crate) fn record_message(
    tx: &Connection,
    task: &TaskRecord,
    m: &MessageRecord,
    summary: Option<&str>,
    drafts: &mut Vec<EventDraft>,
) -> Result<bool, StorageError> {
    if !store::insert_message(tx, m)? {
        return Ok(false);
    }
    drafts.push(draft(
        DomainEvent::TaskMessageRecorded {
            message_id: m.id,
            kind: m.kind.as_str().into(),
            sender: m.sender.clone(),
            assignment_id: m.assignment_id,
            summary: summary.map(|s| bounded(s, 300)),
        },
        &task_context(task),
    )?);
    Ok(true)
}

fn validate_title(title: &str) -> Result<String, ProtocolError> {
    let t = title.trim();
    if t.is_empty() || t.chars().count() > MAX_TITLE {
        return Err(invalid_params(format!("title must be 1 to {MAX_TITLE} characters")));
    }
    Ok(t.to_owned())
}

fn validate_description(d: Option<String>) -> Result<Option<String>, ProtocolError> {
    match d {
        Some(d) if d.trim().is_empty() => Ok(None),
        Some(d) if d.len() > MAX_DESCRIPTION => {
            Err(invalid_params(format!("description exceeds {MAX_DESCRIPTION} bytes")))
        }
        other => Ok(other),
    }
}

fn validate_criteria(c: Vec<String>) -> Result<Vec<String>, ProtocolError> {
    let c: Vec<String> = c.into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    if c.len() > MAX_CRITERIA || c.iter().any(|s| s.chars().count() > MAX_CRITERION) {
        return Err(invalid_params(format!(
            "at most {MAX_CRITERIA} acceptance criteria of at most {MAX_CRITERION} characters"
        )));
    }
    Ok(c)
}

fn validate_note(note: Option<String>) -> Result<Option<String>, ProtocolError> {
    match note {
        Some(n) if n.trim().is_empty() => Ok(None),
        Some(n) if n.len() > MAX_NOTE => Err(invalid_params(format!("note exceeds {MAX_NOTE} bytes"))),
        other => Ok(other),
    }
}

/// Adds `task → depends_on` after checking project, existence and cycles.
fn add_dependency_in_tx(
    tx: &Connection,
    task: &TaskRecord,
    depends_on: Uuid,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<()> {
    let target = check!(lookup(store::get_task(tx, depends_on), ErrorCode::NotFound)?);
    if target.project_id != task.project_id {
        return Ok(Err(invalid_params("a task can only depend on tasks of the same project")));
    }
    if store::dependencies_of(tx, task.id)?.iter().any(|(id, _)| *id == depends_on) {
        return Ok(Err(ProtocolError::new(ErrorCode::Conflict, "the dependency already exists")));
    }
    let edges = store::project_edges(tx, task.project_id)?;
    if would_create_cycle(&edges, task.id, depends_on) {
        return Ok(Err(invalid_params(format!(
            "\"{}\" already depends on \"{}\" (directly or indirectly); this dependency would create a cycle",
            target.title, task.title
        ))));
    }
    store::add_dependency(tx, task.id, depends_on, now)?;
    drafts.push(draft(DomainEvent::TaskDependencyAdded { depends_on }, &task_context(task))?);
    Ok(Ok(()))
}

#[derive(Clone)]
pub(crate) struct TaskService {
    storage: Storage,
    bus: EventBus,
    agents: AgentService,
    workspaces: WorkspaceService,
    /// Sessions Core is stopping after their assignment reported, with their
    /// workspace. A new assignment in that workspace waits for these only;
    /// any other live session there (e.g. one the operator resumed) is never
    /// stopped by Core, and the start fails with `workspace_occupied`.
    stopping: Arc<Mutex<HashMap<Uuid, Uuid>>>,
    /// Assignments being started now; a second concurrent start is refused.
    starting: Arc<Mutex<std::collections::HashSet<Uuid>>>,
}

impl TaskService {
    pub fn new(storage: Storage, bus: EventBus, agents: AgentService, workspaces: WorkspaceService) -> Self {
        Self { storage, bus, agents, workspaces, stopping: Arc::default(), starting: Arc::default() }
    }

    fn stopping(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Uuid>> {
        // A poisoned map only holds session ids; keep using it.
        self.stopping.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until Core has finished stopping its own reported sessions in `workspace_id`.
    async fn wait_for_reported_stops(&self, workspace_id: Uuid) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while self.stopping().values().any(|w| *w == workspace_id) {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(%workspace_id, "a reported session is still stopping; starting anyway");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    async fn wire(&self, id: Uuid) -> Result<Task, ProtocolError> {
        self.storage
            .call(move |c| {
                let t = store::get_task(c, id)?;
                convert::task(c, t)
            })
            .await
            .map_err(errors::storage)
    }

    /// Commits `work`, then returns the wire task `id`.
    async fn commit_task<F>(&self, id: Uuid, work: F) -> Result<Task, ProtocolError>
    where
        F: FnOnce(&Connection, i64, &mut Vec<EventDraft>) -> Checked<()> + Send + 'static,
    {
        commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            check!(work(tx, now_ms(), &mut drafts)?);
            Ok(Ok(((), drafts)))
        })
        .await?;
        self.wire(id).await
    }

    // --- queries ------------------------------------------------------------

    pub async fn list(
        &self,
        project_id: Option<Uuid>,
        include_closed: bool,
        limit: u32,
    ) -> Result<Vec<Task>, ProtocolError> {
        self.storage
            .call(move |c| {
                store::list_tasks(c, project_id, include_closed, limit)?
                    .into_iter()
                    .map(|t| convert::task(c, t))
                    .collect()
            })
            .await
            .map_err(errors::storage)
    }

    pub async fn get(&self, id: Uuid) -> Result<TaskDetail, ProtocolError> {
        self.storage
            .call(move |c| {
                let t = store::get_task(c, id)?;
                let deps = store::dependencies_of(c, id)?;
                let readiness = match readiness(&deps) {
                    Readiness::Satisfied => TaskReadiness::Satisfied,
                    Readiness::Waiting(_) => TaskReadiness::Waiting,
                    Readiness::Broken(_) => TaskReadiness::Broken,
                };
                let dependencies = deps
                    .iter()
                    .map(|(d, _)| store::get_task(c, *d).map(|t| convert::task_ref(&t)))
                    .collect::<Result<_, _>>()?;
                let dependents = store::dependents_of(c, id)?
                    .iter()
                    .map(|d| store::get_task(c, *d).map(|t| convert::task_ref(&t)))
                    .collect::<Result<_, _>>()?;
                Ok(TaskDetail {
                    dependencies,
                    dependents,
                    readiness,
                    assignments: store::assignments_for_task(c, id)?
                        .into_iter()
                        .map(convert::assignment)
                        .collect(),
                    messages: store::messages_for_task(c, id)?.into_iter().map(convert::message).collect(),
                    findings: store::findings_for_task(c, id)?.into_iter().map(convert::finding).collect(),
                    proposals: store::proposals_for_task(c, id)?.into_iter().map(convert::proposal).collect(),
                    task: convert::task(c, t)?,
                })
            })
            .await
            .map_err(errors::storage)
    }

    // --- authoring ----------------------------------------------------------

    pub async fn create(&self, p: TasksCreateParams) -> Result<Task, ProtocolError> {
        let title = validate_title(&p.title)?;
        let description = validate_description(p.description)?;
        let acceptance_criteria = validate_criteria(p.acceptance_criteria)?;
        let id = new_id();
        let record = TaskRecord {
            id,
            project_id: p.project_id,
            parent_task_id: p.parent_task_id,
            title,
            description,
            kind: p.kind.unwrap_or(TaskKind::General),
            priority: p.priority.unwrap_or(Priority::Normal),
            acceptance_criteria,
            status: TaskStatus::Draft,
            status_reason: None,
            result: None,
            failure: None,
            origin: "operator".into(),
            origin_proposal_id: None,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            completed_at_ms: None,
        };
        let (depends_on, publish) = (p.depends_on, p.publish);
        self.commit_task(id, move |tx, now, drafts| {
            check!(lookup(projects::get(tx, record.project_id), ErrorCode::ProjectNotFound)?);
            if let Some(parent) = record.parent_task_id {
                let parent = check!(lookup(store::get_task(tx, parent), ErrorCode::NotFound)?);
                if parent.project_id != record.project_id {
                    return Ok(Err(invalid_params("the parent task belongs to another project")));
                }
            }
            let record = TaskRecord { created_at_ms: now, updated_at_ms: now, ..record };
            check!(insert_task_in_tx(tx, &record, drafts)?);
            for d in depends_on {
                check!(add_dependency_in_tx(tx, &record, d, now, drafts)?);
            }
            if publish {
                let (to, reason) = readiness_status(tx, record.id)?;
                check!(set_status(tx, record.id, to, reason, None, None, now, drafts)?);
            }
            Ok(Ok(()))
        })
        .await
    }

    pub async fn update(&self, p: TasksUpdateParams) -> Result<Task, ProtocolError> {
        let mut fields = Vec::new();
        let edit = TaskEdit {
            title: match p.title {
                Some(t) => {
                    fields.push("title".to_owned());
                    Some(validate_title(&t)?)
                }
                None => None,
            },
            description: match p.description {
                Some(d) => {
                    fields.push("description".to_owned());
                    Some(validate_description(Some(d))?)
                }
                None => None,
            },
            kind: p.kind.inspect(|_| fields.push("kind".to_owned())),
            priority: p.priority.inspect(|_| fields.push("priority".to_owned())),
            acceptance_criteria: match p.acceptance_criteria {
                Some(c) => {
                    fields.push("acceptance_criteria".to_owned());
                    Some(validate_criteria(c)?)
                }
                None => None,
            },
        };
        if fields.is_empty() {
            return Err(invalid_params("nothing to update"));
        }
        let (id, expected) = (p.task_id, p.expected_revision);
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if t.status.is_terminal() || t.status.has_active_work() {
                return Ok(Err(invalid_state(format!("a {} task cannot be edited", t.status))));
            }
            let after = store::edit_task(tx, id, expected, &edit, now)?;
            drafts.push(draft(
                DomainEvent::TaskUpdated { revision: after.revision, fields },
                &task_context(&after),
            )?);
            Ok(Ok(()))
        })
        .await
    }

    /// `draft` → `ready` (or `blocked` while dependencies are open).
    pub async fn publish(&self, id: Uuid) -> Result<Task, ProtocolError> {
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if t.status != TaskStatus::Draft {
                return Ok(Err(invalid_state(format!(
                    "only a draft can be published; the task is {}",
                    t.status
                ))));
            }
            let (to, reason) = readiness_status(tx, id)?;
            check!(set_status(tx, id, to, reason, None, None, now, drafts)?);
            Ok(Ok(()))
        })
        .await
    }

    pub async fn add_dependency(&self, id: Uuid, depends_on: Uuid) -> Result<Task, ProtocolError> {
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if !matches!(
                t.status,
                TaskStatus::Draft | TaskStatus::Ready | TaskStatus::Blocked | TaskStatus::Failed
            ) {
                return Ok(Err(invalid_state(format!(
                    "dependencies can only change before work starts; the task is {}",
                    t.status
                ))));
            }
            check!(add_dependency_in_tx(tx, &t, depends_on, now, drafts)?);
            refresh_readiness(tx, &t, now, drafts)
        })
        .await
    }

    pub async fn remove_dependency(&self, id: Uuid, depends_on: Uuid) -> Result<Task, ProtocolError> {
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if !matches!(
                t.status,
                TaskStatus::Draft | TaskStatus::Ready | TaskStatus::Blocked | TaskStatus::Failed
            ) {
                return Ok(Err(invalid_state(format!(
                    "dependencies can only change before work starts; the task is {}",
                    t.status
                ))));
            }
            if !store::remove_dependency(tx, id, depends_on)? {
                return Ok(Err(ProtocolError::new(ErrorCode::NotFound, "no such dependency")));
            }
            drafts.push(draft(DomainEvent::TaskDependencyRemoved { depends_on }, &task_context(&t))?);
            refresh_readiness(tx, &t, now, drafts)
        })
        .await
    }

    // --- operator decisions -------------------------------------------------

    /// Accepts the reported work: `awaiting_review` → `completed`. Only the
    /// operator can do this; no agent report completes a task.
    pub async fn complete(
        &self,
        id: Uuid,
        note: Option<String>,
        actor: String,
    ) -> Result<Task, ProtocolError> {
        let note = validate_note(note)?;
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if t.status != TaskStatus::AwaitingReview {
                return Ok(Err(invalid_state(format!(
                    "only a task awaiting review can be completed; the task is {}",
                    t.status
                ))));
            }
            let last_report = store::messages_for_task(tx, id)?
                .into_iter()
                .rev()
                .find(|m| m.sender == "agent" && m.payload.get("report").is_some());
            let result = json!({
                "summary": last_report.as_ref().and_then(|m| m.payload["report"]["summary"].as_str()),
                "report": last_report.as_ref().map(|m| m.payload["report"].clone()),
                "report_message_id": last_report.as_ref().map(|m| m.id),
                "accepted_by": actor,
                "note": note,
            });
            store::resolve_findings(tx, id, now)?;
            let m = new_message(
                &t,
                None,
                MessageKind::StatusUpdate,
                "operator",
                None,
                "task",
                json!({ "action": "completed", "by": actor, "note": note }),
                None,
                "recorded",
                now,
            );
            record_message(tx, &t, &m, Some("completed by the operator"), drafts)?;
            check!(set_status(
                tx,
                id,
                TaskStatus::Completed,
                Some(format!("accepted by {actor}")),
                Some(&result),
                None,
                now,
                drafts
            )?);
            Ok(Ok(()))
        })
        .await
    }

    pub async fn fail(&self, id: Uuid, note: Option<String>, actor: String) -> Result<Task, ProtocolError> {
        let note = validate_note(note)?;
        let reason = note.clone().unwrap_or_else(|| format!("failed by {actor}"));
        self.close(id, TaskStatus::Failed, reason, actor).await
    }

    pub async fn cancel(&self, id: Uuid, note: Option<String>, actor: String) -> Result<Task, ProtocolError> {
        let note = validate_note(note)?;
        let reason = note.unwrap_or_else(|| format!("cancelled by {actor}"));
        self.close(id, TaskStatus::Cancelled, reason, actor).await
    }

    /// Fails or cancels a task, cancelling its open assignment (and stopping
    /// that session) first.
    async fn close(
        &self,
        id: Uuid,
        to: TaskStatus,
        reason: String,
        actor: String,
    ) -> Result<Task, ProtocolError> {
        let stop = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            if let Err(e) = t.status.transition(to) {
                return Ok(Err(invalid_state(format!("task is {}; {e}", t.status))));
            }
            let stop = match store::open_assignment(tx, id)? {
                Some(a) => {
                    assign::end_assignment(
                        tx,
                        &t,
                        &a,
                        switchyard_tasks::AssignmentStatus::Cancelled,
                        &reason,
                        now,
                        &mut drafts,
                    )?;
                    a.session_id.filter(|_| a.status == switchyard_tasks::AssignmentStatus::Active)
                }
                None => None,
            };
            let m = new_message(
                &t,
                None,
                MessageKind::StatusUpdate,
                "operator",
                None,
                "task",
                json!({ "action": to.as_str(), "by": actor, "reason": reason }),
                None,
                "recorded",
                now,
            );
            record_message(tx, &t, &m, Some(&reason), &mut drafts)?;
            let failure = (to == TaskStatus::Failed).then_some(reason.as_str());
            check!(set_status(tx, id, to, Some(reason.clone()), None, failure, now, &mut drafts)?);
            Ok(Ok((stop, drafts)))
        })
        .await?;
        if let Some(session) = stop {
            self.stop_session(session).await;
        }
        self.wire(id).await
    }

    /// `failed` → `ready`/`blocked`, or `blocked` → `ready` once nothing
    /// blocks it any more.
    pub async fn retry(&self, id: Uuid, note: Option<String>, actor: String) -> Result<Task, ProtocolError> {
        let note = validate_note(note)?;
        self.commit_task(id, move |tx, now, drafts| {
            let t = check!(lookup(store::get_task(tx, id), ErrorCode::NotFound)?);
            let (to, reason) = readiness_status(tx, id)?;
            match (t.status, to) {
                (TaskStatus::Failed, _) | (TaskStatus::Blocked, TaskStatus::Ready) => {}
                (TaskStatus::Blocked, _) => {
                    return Ok(Err(invalid_state("the task's dependencies are still not completed")));
                }
                (s, _) => {
                    return Ok(Err(invalid_state(format!(
                        "only a failed or blocked task can be retried; it is {s}"
                    ))));
                }
            }
            let m = new_message(
                &t,
                None,
                MessageKind::StatusUpdate,
                "operator",
                None,
                "task",
                json!({ "action": "retry", "by": actor, "note": note }),
                None,
                "recorded",
                now,
            );
            record_message(tx, &t, &m, Some("retried by the operator"), drafts)?;
            check!(set_status(tx, id, to, reason, None, None, now, drafts)?);
            Ok(Ok(()))
        })
        .await
    }

    /// Approves (creating a draft task) or rejects a subtask an agent proposed.
    pub async fn decide_proposal(
        &self,
        proposal_id: Uuid,
        approve: bool,
        note: Option<String>,
        actor: String,
    ) -> Result<TasksDecideProposalResult, ProtocolError> {
        let note = validate_note(note)?;
        let (proposal, created) = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let p = check!(lookup(store::get_proposal(tx, proposal_id), ErrorCode::NotFound)?);
            if p.status != ProposalStatus::Pending {
                return Ok(Err(ProtocolError::new(
                    ErrorCode::Conflict,
                    format!("the proposal is already {}", p.status),
                )));
            }
            let parent = store::get_task(tx, p.task_id)?;
            let mut created = None;
            if approve {
                let sub: ProposedSubtask = match serde_json::from_value(p.proposal.clone()) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(Err(invalid_state(format!("the stored proposal is not valid: {e}"))));
                    }
                };
                // Resolve dependencies first: earlier subtasks must already be approved.
                let siblings = store::proposals_for_task(tx, p.task_id)?;
                let mut deps = Vec::new();
                for d in &sub.depends_on {
                    match d {
                        SubtaskDependency::Index(i) => {
                            let sibling = siblings
                                .iter()
                                .find(|s| s.message_id == p.message_id && s.position as usize == *i);
                            match sibling.and_then(|s| s.created_task_id) {
                                Some(id) => deps.push(id),
                                None => {
                                    return Ok(Err(invalid_state(format!(
                                        "this subtask depends on proposed subtask {i}; approve that one first"
                                    ))));
                                }
                            }
                        }
                        SubtaskDependency::Task(id) => deps.push(*id),
                    }
                }
                let title = check!(validate_title(&sub.title));
                let record = TaskRecord {
                    id: new_id(),
                    project_id: p.project_id,
                    parent_task_id: Some(p.task_id),
                    title,
                    description: check!(validate_description(sub.description.clone())),
                    kind: sub.kind,
                    priority: parent.priority,
                    acceptance_criteria: check!(validate_criteria(sub.acceptance_criteria.clone())),
                    status: TaskStatus::Draft,
                    status_reason: None,
                    result: None,
                    failure: None,
                    origin: "proposal".into(),
                    origin_proposal_id: Some(p.id),
                    revision: 1,
                    created_at_ms: now,
                    updated_at_ms: now,
                    completed_at_ms: None,
                };
                check!(insert_task_in_tx(tx, &record, &mut drafts)?);
                for d in deps {
                    check!(add_dependency_in_tx(tx, &record, d, now, &mut drafts)?);
                }
                created = Some(record.id);
            }
            let status = if approve { ProposalStatus::Approved } else { ProposalStatus::Rejected };
            let decided =
                store::decide_proposal(tx, proposal_id, status, &actor, note.as_deref(), created, now)?;
            drafts.push(draft(
                DomainEvent::TaskProposalDecided {
                    proposal_id,
                    decision: status.as_str().into(),
                    created_task_id: created,
                },
                &task_context(&parent),
            )?);
            Ok(Ok(((convert::proposal(decided), created), drafts)))
        })
        .await?;
        let created_task = match created {
            Some(id) => Some(self.wire(id).await?),
            None => None,
        };
        Ok(TasksDecideProposalResult { proposal, created_task })
    }

    async fn stop_session(&self, session_id: Uuid) {
        if self.agents.is_attached(session_id)
            && let Err(err) = self.agents.stop(session_id).await
        {
            tracing::warn!(%session_id, ?err, "cannot stop the session of an ended assignment");
        }
    }
}

fn insert_task_in_tx(tx: &Connection, record: &TaskRecord, drafts: &mut Vec<EventDraft>) -> Checked<()> {
    store::insert_task(tx, record)?;
    drafts.push(draft(
        DomainEvent::TaskCreated {
            title: record.title.clone(),
            kind: record.kind.as_str().into(),
            priority: record.priority.as_str().into(),
            status: record.status.as_str().into(),
            origin: record.origin.clone(),
            parent_task_id: record.parent_task_id,
            origin_proposal_id: record.origin_proposal_id,
        },
        &task_context(record),
    )?);
    Ok(Ok(()))
}

/// After a dependency change: `ready` ↔ `blocked` to match the dependencies.
fn refresh_readiness(tx: &Connection, t: &TaskRecord, now: i64, drafts: &mut Vec<EventDraft>) -> Checked<()> {
    let current = store::get_task(tx, t.id)?;
    let (want, reason) = readiness_status(tx, t.id)?;
    let blocked_by_dependencies =
        current.status_reason.as_deref().is_some_and(|r| r.starts_with(DEPENDENCY_REASON));
    match (current.status, want) {
        (TaskStatus::Ready, TaskStatus::Blocked) => {
            check!(set_status(tx, t.id, want, reason, None, None, now, drafts)?);
        }
        (TaskStatus::Blocked, TaskStatus::Ready) if blocked_by_dependencies => {
            check!(set_status(tx, t.id, want, None, None, None, now, drafts)?);
        }
        _ => {}
    }
    Ok(Ok(()))
}
