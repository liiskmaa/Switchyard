//! Assignments: giving one role on a task to an agent, starting its session
//! with the task context, cancelling it, and operator notes.

use std::path::PathBuf;

use serde_json::json;
use switchyard_agents::{AgentProvider, SessionAccess};
use switchyard_events::{DomainEvent, EventDraft, new_id, now_ms};
use switchyard_protocol::methods::{TaskAssignmentResult, TasksAssignParams, TasksNoteResult};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::tasks::{self as store, AssignmentRecord, TaskRecord};
use switchyard_storage::{Connection, StorageError, events, projects, sessions};
use switchyard_tasks::{
    AssignmentStatus, CONTEXT_VERSION, ContextDependency, ContextFinding, MessageKind, Role, TaskContext,
    TaskStatus,
};
use uuid::Uuid;

use super::{
    Checked, TaskService, check, convert, invalid_params, invalid_state, lookup, new_message, record_message,
    set_status, task_context,
};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

/// Where a task goes back to when an assignment ends without a report: a
/// review returns to `awaiting_review`; other work to `awaiting_review` if
/// earlier work was reported, else to `ready`.
fn return_status(tx: &Connection, task: &TaskRecord, role: Role) -> Result<TaskStatus, StorageError> {
    if role == Role::Review {
        return Ok(TaskStatus::AwaitingReview);
    }
    let reported = store::assignments_for_task(tx, task.id)?
        .iter()
        .any(|a| a.status == AssignmentStatus::Reported && a.role != Role::Review);
    Ok(if reported { TaskStatus::AwaitingReview } else { TaskStatus::Ready })
}

/// Ends an open assignment (`reported`, `failed` or `cancelled`) and journals it.
pub(super) fn end_assignment(
    tx: &Connection,
    task: &TaskRecord,
    a: &AssignmentRecord,
    to: AssignmentStatus,
    outcome: &str,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Result<AssignmentRecord, StorageError> {
    let ended = store::set_assignment_status(
        tx,
        a.id,
        &[AssignmentStatus::Pending, AssignmentStatus::Active],
        to,
        Some(outcome),
        now,
    )?;
    drafts.push(draft(
        DomainEvent::TaskAssignmentEnded {
            assignment_id: a.id,
            status: to.as_str().into(),
            outcome: Some(super::bounded(outcome, 500)),
        },
        &EventContext { workspace_id: Some(a.workspace_id), ..task_context(task) },
    )?);
    Ok(ended)
}

use switchyard_events::EventContext;

/// Ends an assignment that did not report and moves its task back.
pub(super) fn abandon_assignment(
    tx: &Connection,
    assignment_id: Uuid,
    to: AssignmentStatus,
    outcome: &str,
    fail_task: bool,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<AssignmentRecord> {
    let a = store::get_assignment(tx, assignment_id)?;
    if !a.status.is_open() {
        return Ok(Err(invalid_state(format!("the assignment is already {}", a.status))));
    }
    let t = store::get_task(tx, a.task_id)?;
    let ended = end_assignment(tx, &t, &a, to, outcome, now, drafts)?;
    let next = match t.status {
        TaskStatus::InProgress if fail_task => TaskStatus::Failed,
        TaskStatus::Assigned | TaskStatus::InProgress | TaskStatus::InReview => {
            return_status(tx, &t, a.role)?
        }
        _ => return Ok(Ok(ended)),
    };
    let failure = (next == TaskStatus::Failed).then_some(outcome);
    check!(set_status(tx, t.id, next, Some(super::bounded(outcome, 500)), None, failure, now, drafts)?);
    Ok(Ok(ended))
}

/// What a new assignment is (an operator's or a workflow step's).
pub(super) struct NewAssignment {
    pub task_id: Uuid,
    pub role: Role,
    pub provider: String,
    pub model: Option<String>,
    pub workspace_id: Uuid,
    pub workspace_project: Uuid,
    /// `operator` or `workflow`.
    pub origin: &'static str,
    pub step_key: Option<String>,
    pub by: String,
    /// Review work in an agent organization (ADR 0035): it reviews the shared
    /// workspace, not earlier work on its own task, so it starts from `ready`.
    pub workspace_review: bool,
}

/// The capabilities every workflow role needs from a provider. All roles
/// need tool permission requests routed to Switchyard: that is how policies
/// apply and how review sessions are kept read-only (ADR 0027).
pub(super) fn role_capabilities(role: Role, caps: &switchyard_agents::Capabilities) -> Result<(), String> {
    if !caps.permission_requests.is_supported() {
        return Err(format!(
            "it does not route permission requests to Switchyard, so it cannot {} on a task",
            role.as_str()
        ));
    }
    Ok(())
}

/// Creates a pending assignment and moves the task to `assigned`, in `tx`.
/// A workflow step that already has an assignment is refused (`step_key` is unique).
pub(super) fn create_assignment(
    tx: &Connection,
    n: &NewAssignment,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<Uuid> {
    let t = check!(lookup(store::get_task(tx, n.task_id), ErrorCode::NotFound)?);
    if n.workspace_project != t.project_id {
        return Ok(Err(invalid_params("the workspace belongs to another project")));
    }
    let allowed = match n.role {
        Role::Review if n.workspace_review => {
            matches!(t.status, TaskStatus::Ready | TaskStatus::AwaitingReview)
        }
        Role::Review => t.status == TaskStatus::AwaitingReview,
        Role::Investigate | Role::Implement => {
            matches!(t.status, TaskStatus::Ready | TaskStatus::AwaitingReview)
        }
    };
    if !allowed {
        let need = if n.role == Role::Review { "awaiting_review" } else { "ready or awaiting_review" };
        return Ok(Err(invalid_state(format!(
            "a {} assignment needs a task that is {need}; the task is {}",
            n.role, t.status
        ))));
    }
    let others = store::open_assignments_on_workspace(tx, n.workspace_id, n.task_id)?;
    if let Some((other, other_role)) = others.first() {
        return Ok(Err(ProtocolError::new(
            ErrorCode::Conflict,
            format!(
                "workspace is in use by a {other_role} assignment of task {other}; one task works in a workspace at a \
                 time"
            ),
        )
        .with_data(json!({ "task_id": other }))));
    }
    let id = new_id();
    let record = AssignmentRecord {
        id,
        task_id: n.task_id,
        project_id: t.project_id,
        role: n.role,
        provider: n.provider.clone(),
        workspace_id: n.workspace_id,
        session_id: None,
        status: AssignmentStatus::Pending,
        context: None,
        context_version: None,
        review_fingerprint: None,
        last_event_seq: 0,
        outcome: None,
        created_at_ms: now,
        started_at_ms: None,
        ended_at_ms: None,
        origin: n.origin.into(),
        step_key: n.step_key.clone(),
        model: n.model.clone(),
    };
    match store::insert_assignment(tx, &record) {
        Ok(()) => {}
        Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
        Err(e) => return Err(e),
    }
    drafts.push(draft(
        DomainEvent::TaskAssigned {
            assignment_id: id,
            role: n.role.as_str().into(),
            provider: n.provider.clone(),
            workspace_id: n.workspace_id,
            model: n.model.clone(),
            step_key: n.step_key.clone(),
        },
        &EventContext { workspace_id: Some(n.workspace_id), ..task_context(&t) },
    )?);
    let m = new_message(
        &t,
        Some(id),
        MessageKind::TaskAssignment,
        if n.origin == "workflow" { "core" } else { "operator" },
        None,
        "agent",
        json!({ "role": n.role, "provider": n.provider, "model": n.model, "workspace_id": n.workspace_id,
                "by": n.by, "step_key": n.step_key }),
        None,
        "recorded",
        now,
    );
    let summary = match &n.model {
        Some(model) => format!("{} assigned to {} ({model})", n.role, n.provider),
        None => format!("{} assigned to {}", n.role, n.provider),
    };
    record_message(tx, &t, &m, Some(&summary), drafts)?;
    check!(set_status(tx, n.task_id, TaskStatus::Assigned, Some(summary), None, None, now, drafts)?);
    Ok(Ok(id))
}

fn access_for(role: Role) -> SessionAccess {
    if role == Role::Review { SessionAccess::Review } else { SessionAccess::Write }
}

fn parse_provider(s: &str) -> Result<AgentProvider, ProtocolError> {
    s.parse::<AgentProvider>()
        .map_err(|_| invalid_params(format!("unknown provider {s:?}; expected \"claude_code\" or \"codex\"")))
}

/// A string fingerprint of every entry in the workspace (`lstat` scan).
pub(super) async fn workspace_fingerprint(path: &str) -> Result<String, ProtocolError> {
    let root = PathBuf::from(path);
    let summary = tokio::task::spawn_blocking(move || {
        crate::workspaces::fingerprint::scan(&root, crate::workspaces::fingerprint::MAX_ENTRIES)
    })
    .await
    .map_err(|e| errors::internal("workspace scan failed", e))?;
    Ok(format!("{:016x}/{}/{}", summary.fingerprint, summary.entries, summary.bytes))
}

impl TaskService {
    pub async fn assign(
        &self,
        p: TasksAssignParams,
        actor: String,
    ) -> Result<TaskAssignmentResult, ProtocolError> {
        let provider = parse_provider(&p.provider)?;
        self.check_provider(provider, p.role).await?;
        self.agents.validate_model(provider, p.model.as_deref()).await?;
        let workspace = self.workspaces.record(p.workspace_id).await?;
        let new = NewAssignment {
            task_id: p.task_id,
            role: p.role,
            provider: provider.as_str().into(),
            model: p.model,
            workspace_id: p.workspace_id,
            workspace_project: workspace.project_id,
            origin: "operator",
            step_key: None,
            by: actor,
            workspace_review: false,
        };
        let assignment_id = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            // An operator assignment is a takeover: never while a workflow drives the task.
            if let Some(w) = switchyard_storage::workflows::find_workflow(tx, new.task_id)?
                && matches!(w.status.as_str(), "pending" | "running")
            {
                return Ok(Err(invalid_state(
                    "the task's workflow is running; pause or cancel it before assigning manually",
                )));
            }
            let id = check!(create_assignment(tx, &new, now_ms(), &mut drafts)?);
            Ok(Ok((id, drafts)))
        })
        .await?;
        if p.start {
            return self.start_assignment(assignment_id).await;
        }
        self.assignment_result(assignment_id).await
    }

    /// Refuses a provider that is not installed or cannot do `role` (its
    /// declared capabilities, ADR 0022/0033).
    pub(super) async fn check_provider(
        &self,
        provider: AgentProvider,
        role: Role,
    ) -> Result<(), ProtocolError> {
        let availability = self.agents.availability(provider, false).await;
        if let Some(error) = availability.error.clone().or_else(|| {
            (!availability.installed).then(|| format!("{} is not installed", availability.display_name))
        }) {
            return Err(ProtocolError::new(ErrorCode::AgentUnavailable, error));
        }
        role_capabilities(role, &switchyard_agents::capabilities(provider)).map_err(|m| {
            ProtocolError::new(ErrorCode::Unsupported, format!("{}: {m}", availability.display_name))
        })
    }

    async fn assignment_result(&self, id: Uuid) -> Result<TaskAssignmentResult, ProtocolError> {
        let (task, assignment) = self
            .storage
            .call(move |c| {
                let a = store::get_assignment(c, id)?;
                let t = store::get_task(c, a.task_id)?;
                Ok((convert::task(c, t)?, convert::assignment(c, a)?))
            })
            .await
            .map_err(errors::storage)?;
        Ok(TaskAssignmentResult { task, assignment })
    }

    /// Starts (or resumes) the assignment's session, records the context it
    /// was given, then sends the context as the first message.
    pub async fn start_assignment(&self, assignment_id: Uuid) -> Result<TaskAssignmentResult, ProtocolError> {
        if !self.starting.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(assignment_id) {
            return Err(ProtocolError::new(ErrorCode::Conflict, "the assignment is already being started"));
        }
        let result = self.start_assignment_once(assignment_id).await;
        self.starting.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&assignment_id);
        result
    }

    async fn start_assignment_once(
        &self,
        assignment_id: Uuid,
    ) -> Result<TaskAssignmentResult, ProtocolError> {
        type Prepared = (AssignmentRecord, TaskRecord, TaskContext, Vec<Uuid>, Option<Uuid>);
        let (a, t, ctx, note_ids, reuse): Prepared = self
            .storage
            .call(move |c| -> Result<Result<Prepared, ProtocolError>, StorageError> {
                let a = match store::get_assignment(c, assignment_id) {
                    Ok(a) => a,
                    Err(StorageError::NotFound(w)) => {
                        return Ok(Err(ProtocolError::new(ErrorCode::NotFound, format!("{w} not found"))));
                    }
                    Err(e) => return Err(e),
                };
                if a.status != AssignmentStatus::Pending {
                    return Ok(Err(invalid_state(format!("the assignment is {}, not pending", a.status))));
                }
                let t = store::get_task(c, a.task_id)?;
                if t.status != TaskStatus::Assigned {
                    return Ok(Err(invalid_state(format!("the task is {}, not assigned", t.status))));
                }
                let ws = switchyard_storage::workspaces::get(c, a.workspace_id)?;
                let project = projects::get(c, t.project_id)?;
                let mut dependencies = Vec::new();
                for (id, _) in store::dependencies_of(c, t.id)? {
                    let d = store::get_task(c, id)?;
                    dependencies.push(ContextDependency {
                        task_id: d.id,
                        title: d.title.clone(),
                        status: d.status,
                        result_summary: d
                            .result
                            .as_ref()
                            .and_then(|r| r["summary"].as_str())
                            .map(|s| super::bounded(s, 2000)),
                    });
                }
                // Review triage (ADR 0038): the reasoning step sees the review's
                // findings; the next implementation or review sees its answer.
                let review_triage = a.role == Role::Investigate
                    && a.step_key.as_deref().is_some_and(switchyard_tasks::workflow::is_triage_step);
                let reasoning = if matches!(a.role, Role::Implement | Role::Review) {
                    latest_reasoning(c, t.id)?
                } else {
                    None
                };
                let review_findings = if a.role == Role::Implement || review_triage {
                    store::findings_for_task(c, t.id)?
                        .into_iter()
                        .filter(|f| f.status == "open")
                        .map(|f| ContextFinding {
                            severity: f.severity,
                            title: f.title,
                            path: f.path,
                            line: f.line,
                            explanation: f.explanation,
                            suggestion: f.suggestion,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let notes = store::pending_notes(c, t.id)?;
                let ctx = TaskContext {
                    version: CONTEXT_VERSION,
                    task_id: t.id,
                    assignment_id: a.id,
                    role: a.role,
                    title: t.title.clone(),
                    description: t.description.clone(),
                    kind: t.kind,
                    priority: t.priority,
                    acceptance_criteria: t.acceptance_criteria.clone(),
                    project_name: project.name,
                    workspace_path: ws.path,
                    workspace_branch: ws.branch,
                    dependencies,
                    review_findings,
                    operator_notes: notes
                        .iter()
                        .filter_map(|m| m.payload["text"].as_str().map(str::to_owned))
                        .collect(),
                    restrictions: TaskContext::standard_restrictions(a.role),
                    organization: super::organization::brief_for(c, &t)?,
                    review_triage,
                    reasoning,
                    subtask: subtask_of(c, t.id, a.step_key.as_deref())?,
                    test_results: if matches!(a.role, Role::Review | Role::Investigate | Role::Implement)
                        && a.origin == "workflow"
                    {
                        super::tests::latest_for_context(c, t.id)?
                    } else {
                        None
                    },
                };
                // Continue the conversation of this task's previous session for
                // the same role, provider and workspace, when it can be resumed.
                let reuse = store::assignments_for_task(c, t.id)?
                    .into_iter()
                    .rev()
                    .filter(|p| {
                        p.id != a.id
                            && p.role == a.role
                            && p.provider == a.provider
                            && p.model == a.model
                            && p.workspace_id == a.workspace_id
                    })
                    .find_map(|p| p.session_id)
                    .map(|s| sessions::get(c, s))
                    .transpose()?
                    .filter(|s| {
                        s.state.is_terminal()
                            && s.access == access_for(a.role)
                            && switchyard_agents::capabilities(s.provider).resume_session.is_supported()
                    })
                    .map(|s| s.id);
                Ok(Ok((a, t, ctx, notes.iter().map(|m| m.id).collect(), reuse)))
            })
            .await
            .map_err(errors::storage)??;

        let provider = parse_provider(&a.provider)?;
        let access = access_for(a.role);
        let context_json =
            serde_json::to_value(&ctx).map_err(|e| errors::internal("cannot encode the task context", e))?;
        let prompt = switchyard_tasks::render(&ctx);

        // Core may still be stopping the session of the assignment that just
        // reported here: wait for that stop only. Core never stops any other
        // session (for example one the operator resumed); then the start fails
        // with `workspace_occupied`.
        self.wait_for_reported_stops(a.workspace_id).await;
        // Taken after that stop, so only changes during this review count.
        let fingerprint = match a.role {
            Role::Review => Some(workspace_fingerprint(&ctx.workspace_path).await?),
            _ => None,
        };
        // The exact content under review, as a Git tree (ADR 0029): an
        // integration later requires the delivered tree to equal it.
        let review_tree = match a.role {
            Role::Review => {
                let w = self.workspaces.record(a.workspace_id).await?;
                Some(
                    self.workspaces
                        .content_tree(&w)
                        .await
                        .map_err(|e| errors::internal("cannot record the reviewed content", e))?,
                )
            }
            _ => None,
        };
        let started = match reuse {
            Some(session_id) => self.agents.resume(session_id, None).await.map(|s| (s, true)),
            None => self
                .agents
                // The assignment's model goes to the native adapter exactly
                // (`--model` for Claude, `model` on Codex's thread/start).
                .start(provider, access, a.workspace_id, Some(t.project_id), None, a.model.clone())
                .await
                .map(|s| (s, false)),
        };
        let (session, resumed) = match started {
            Ok(s) => s,
            Err(err) => {
                self.give_up(a.id, None, &format!("the session could not be started: {}", err.message)).await;
                return Err(err);
            }
        };
        let session_id = session.id;
        let committed = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let after_seq = events::last_seq(tx)?.unwrap_or(0);
            let a = match store::start_assignment(
                tx,
                a.id,
                session_id,
                &context_json,
                CONTEXT_VERSION,
                fingerprint.as_deref(),
                after_seq,
                now,
            ) {
                Ok(a) => a,
                Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
                Err(e) => return Err(e),
            };
            store::mark_delivered(tx, &note_ids, a.id, now)?;
            if let Some(tree) = &review_tree {
                switchyard_storage::integrations::set_review_tree(tx, a.id, tree)?;
            }
            drafts.push(draft(
                DomainEvent::TaskAssignmentStarted {
                    assignment_id: a.id,
                    session_id,
                    access: access.as_str().into(),
                    context_version: CONTEXT_VERSION,
                    resumed,
                },
                &EventContext { workspace_id: Some(a.workspace_id), ..task_context(&t) },
            )?);
            let to = if a.role == Role::Review { TaskStatus::InReview } else { TaskStatus::InProgress };
            check!(set_status(
                tx,
                t.id,
                to,
                Some(format!("{} session started", a.role)),
                None,
                None,
                now,
                &mut drafts
            )?);
            Ok(Ok(((), drafts)))
        })
        .await;
        if let Err(err) = committed {
            self.stop_session(session_id).await;
            // Ends the assignment only if it is still pending: if another
            // start won the race, its assignment and session are left alone.
            self.give_up(
                assignment_id,
                None,
                &format!("the assignment could not be recorded: {}", err.message),
            )
            .await;
            return Err(err);
        }
        if let Err(err) = self.agents.send(session_id, prompt, false).await {
            self.give_up(
                assignment_id,
                Some(session_id),
                &format!("the task context was not delivered: {}", err.message),
            )
            .await;
            self.stop_session(session_id).await;
            return Err(err);
        }
        self.assignment_result(assignment_id).await
    }

    /// Ends an open assignment without a report (logging, never failing the caller).
    /// Ends an assignment this start call owns: still pending without a
    /// session (`owned_session` None), or active with exactly `owned_session`.
    /// Anything else belongs to a concurrent start and is left alone.
    pub(super) async fn give_up(&self, assignment_id: Uuid, owned_session: Option<Uuid>, outcome: &str) {
        let outcome = outcome.to_owned();
        let r = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            let current = store::get_assignment(tx, assignment_id)?;
            let owned = match owned_session {
                None => current.status == AssignmentStatus::Pending && current.session_id.is_none(),
                Some(s) => current.status == AssignmentStatus::Active && current.session_id == Some(s),
            };
            if !owned {
                return Ok(Ok(((), drafts)));
            }
            check!(abandon_assignment(
                tx,
                assignment_id,
                AssignmentStatus::Failed,
                &outcome,
                false,
                now_ms(),
                &mut drafts
            )?);
            Ok(Ok(((), drafts)))
        })
        .await;
        if let Err(err) = r {
            tracing::warn!(%assignment_id, ?err, "cannot end the assignment");
        }
    }

    /// The operator cancels an open assignment; its session is stopped.
    pub async fn cancel_assignment(
        &self,
        assignment_id: Uuid,
        reason: Option<String>,
        actor: String,
    ) -> Result<TaskAssignmentResult, ProtocolError> {
        let outcome =
            reason.filter(|r| !r.trim().is_empty()).unwrap_or_else(|| format!("cancelled by {actor}"));
        let stop = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            let a = check!(lookup(store::get_assignment(tx, assignment_id), ErrorCode::NotFound)?);
            let ended = check!(abandon_assignment(
                tx,
                assignment_id,
                AssignmentStatus::Cancelled,
                &outcome,
                false,
                now_ms(),
                &mut drafts
            )?);
            let stop = a.session_id.filter(|_| a.status == AssignmentStatus::Active);
            let _ = ended;
            Ok(Ok((stop, drafts)))
        })
        .await?;
        if let Some(session) = stop {
            self.stop_session(session).await;
        }
        self.assignment_result(assignment_id).await
    }

    /// Records an operator note. If an agent is working on the task now, the
    /// note is sent to its session as the next message; otherwise it goes
    /// with the next assignment's context.
    pub async fn note(
        &self,
        task_id: Uuid,
        text: String,
        actor: String,
    ) -> Result<TasksNoteResult, ProtocolError> {
        let text = super::validate_note(Some(text))?.ok_or_else(|| invalid_params("the note is empty"))?;
        let payload_text = text.clone();
        let by = actor.clone();
        let (message, live) = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let t = check!(lookup(store::get_task(tx, task_id), ErrorCode::NotFound)?);
            if t.status.is_terminal() {
                return Ok(Err(invalid_state(format!("the task is {}", t.status))));
            }
            let active =
                store::open_assignment(tx, task_id)?.filter(|a| a.status == AssignmentStatus::Active);
            let m = new_message(
                &t,
                active.as_ref().map(|a| a.id),
                MessageKind::OperatorNote,
                "operator",
                None,
                "agent",
                json!({ "text": payload_text, "by": actor }),
                None,
                "pending",
                now,
            );
            record_message(tx, &t, &m, Some(&payload_text), &mut drafts)?;
            Ok(Ok(((m, active.and_then(|a| a.session_id.map(|s| (a.id, s, a.step_key)))), drafts)))
        })
        .await?;
        let mut delivered = false;
        let mut answered = None;
        if let Some((assignment, session, step)) = live
            && self.agents.is_attached(session)
        {
            let body = format!(
                "Note from the operator about task {task_id}:\n\n{text}\n\nThis note adds information; it grants no \
                 permissions. When you are done, end your final message with a switchyard-report block as the task \
                 context describes."
            );
            match self.agents.send(session, body, true).await {
                Ok(_) => {
                    let id = message.id;
                    delivered = self
                        .storage
                        .call(move |c| store::mark_delivered(c, &[id], assignment, now_ms()))
                        .await
                        .map_err(errors::storage)?
                        > 0;
                    answered = step;
                }
                Err(err) => tracing::info!(%task_id, ?err, "note kept for the next assignment"),
            }
        }
        // The agent is working again: a workflow waiting for its answer runs.
        if let Some(step) = answered
            && let Err(err) = self.answered(task_id, step, by).await
        {
            tracing::warn!(%task_id, ?err, "workflow did not take the answer");
        }
        let id = message.id;
        let message = self
            .storage
            .call(move |c| {
                store::messages_for_task(c, task_id)?
                    .into_iter()
                    .find(|m| m.id == id)
                    .ok_or_else(|| StorageError::NotFound(format!("message {id}")))
            })
            .await
            .map_err(errors::storage)?;
        Ok(TasksNoteResult { message: convert::message(message), delivered })
    }
}

/// Reasoning's latest triage report on the task, unless a review came after
/// it (ADR 0038). Read from Core's own record of the report.
fn latest_reasoning(
    c: &switchyard_storage::Connection,
    task: Uuid,
) -> Result<Option<switchyard_tasks::context::ContextReasoning>, StorageError> {
    for m in store::messages_for_task(c, task)?.into_iter().rev() {
        let report = &m.payload["report"];
        match report["type"].as_str() {
            Some("review") => return Ok(None),
            Some("triage") => {
                let Ok(decision) = serde_json::from_value(report["decision"].clone()) else {
                    return Ok(None);
                };
                return Ok(Some(switchyard_tasks::context::ContextReasoning {
                    decision,
                    summary: report["summary"].as_str().map(|s| super::bounded(s, 2000)).unwrap_or_default(),
                    details: report["details"].as_str().map(|s| super::bounded(s, 4000)),
                }));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The planned subtask a workflow step works on (ADR 0040), from the
/// workflow's own state.
fn subtask_of(
    c: &switchyard_storage::Connection,
    task: Uuid,
    step_key: Option<&str>,
) -> Result<Option<switchyard_tasks::context::ContextSubtask>, StorageError> {
    let Some((Some(n), _)) = step_key.map(switchyard_tasks::workflow::split_subtask) else { return Ok(None) };
    let Some(w) = switchyard_storage::workflows::find_workflow(c, task)? else { return Ok(None) };
    let state: switchyard_tasks::workflow::WorkflowState =
        serde_json::from_value(w.state).map_err(|e| StorageError::Corrupt(format!("workflow state: {e}")))?;
    let Some(s) = state.plan.get(n.saturating_sub(1) as usize) else { return Ok(None) };
    Ok(Some(switchyard_tasks::context::ContextSubtask {
        number: n,
        of: u32::try_from(state.plan.len()).unwrap_or(u32::MAX),
        title: s.title.clone(),
        description: s.description.clone(),
        acceptance_criteria: s.acceptance_criteria.clone(),
        done: state.plan.iter().filter(|p| p.done).map(|p| p.title.clone()).collect(),
    }))
}
