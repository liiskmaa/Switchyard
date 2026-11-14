//! The workflow driver (milestone 8.1, ADR 0033).
//!
//! A workflow run makes one task progress through `engineering@1` without
//! the operator creating assignments: investigation → implementation →
//! review → bounded correction rounds → the human delivery gate.
//!
//! * The decisions are pure ([`switchyard_tasks::workflow::decide`]).
//! * This driver turns task events into decision inputs. It follows the
//!   journal like the task reactor, and records the highest event it
//!   processed (`last_event_seq`) in the same transaction as the decision,
//!   so a replayed event is applied once.
//! * A decision, its `workflow.*` events and the assignment of the next step
//!   (with its unique step key) are one transaction. Only after the commit
//!   does Core start the session, send a repair note or cancel an
//!   assignment. A step whose assignment already exists is never created
//!   twice.
//! * Assignments go through the same code as the operator's
//!   ([`super::assign::create_assignment`], `start_assignment`), so policies,
//!   approvals, read-only reviews and workspace rules apply unchanged.
//!
//! Delivery stays the operator's: an approved review ends the workflow at
//! the delivery gate; it never completes the task or integrates anything.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use switchyard_agents::AgentProvider;
use switchyard_events::{DomainEvent, EventDraft, EventEnvelope, EventFilter, RecvError, now_ms};
use switchyard_protocol::methods::WorkflowsStartParams;
use switchyard_protocol::model::{
    RoleBinding, RoleBindings, RoleBindingsPatch, WorkflowInfo, WorkflowSettingsInfo,
};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::tasks as store;
use switchyard_storage::workflows::{self as wf_store, WorkflowRecord, WorkflowUpdate};
use switchyard_storage::{Connection, StorageError};
use switchyard_tasks::workflow::{
    self as wf, Action, Binding, Change, Input, MAX_CORRECTIONS, Outcome, Resume, Step, WorkflowState,
    WorkflowStatus,
};
use switchyard_tasks::{AssignmentStatus, TaskStatus};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::assign::{NewAssignment, create_assignment};
use super::{TaskService, check, invalid_params, invalid_state, lookup, task_context};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};
use crate::state::wait_shutdown;

/// Events the driver follows.
const WATCHED: [&str; 3] = ["task.assignment_ended", "task.message_recorded", "task.status_changed"];

/// Switchyard's defaults while a project has saved no bindings. They are
/// shown as such; the CLI default model is an explicit choice (`None`).
pub(crate) fn default_bindings() -> RoleBindings {
    let claude = || RoleBinding { provider: AgentProvider::ClaudeCode.as_str().into(), model: None };
    RoleBindings {
        investigate: claude(),
        implement: RoleBinding { provider: AgentProvider::Codex.as_str().into(), model: None },
        review: claude(),
        correct: None,
    }
}

fn binding_for(b: &RoleBindings, which: Binding) -> &RoleBinding {
    match which {
        Binding::Investigate => &b.investigate,
        Binding::Implement => &b.implement,
        Binding::Review => &b.review,
        Binding::Correct => b.correct.as_ref().unwrap_or(&b.implement),
    }
}

fn apply_patch(mut b: RoleBindings, p: RoleBindingsPatch) -> RoleBindings {
    if let Some(x) = p.investigate {
        b.investigate = x;
    }
    if let Some(x) = p.implement {
        b.implement = x;
    }
    if let Some(x) = p.review {
        b.review = x;
    }
    if let Some(x) = p.correct {
        b.correct = Some(x);
    }
    b
}

/// Warnings about bindings that are allowed but worth knowing.
fn warnings_for(b: &RoleBindings) -> Vec<String> {
    let mut out = Vec::new();
    let review = &b.review.provider;
    let correct = binding_for(b, Binding::Correct);
    if &b.implement.provider == review || &correct.provider == review {
        out.push(format!(
            "Implementation and review both use {}: the review is less independent. Consider a different provider \
             for review.",
            switchyard_agents::display_name(review.parse().unwrap_or(AgentProvider::ClaudeCode))
        ));
    }
    out
}

fn corrupt(e: serde_json::Error) -> StorageError {
    StorageError::Corrupt(format!("workflow state: {e}"))
}

fn state_of(w: &WorkflowRecord) -> Result<WorkflowState, StorageError> {
    serde_json::from_value(w.state.clone()).map_err(corrupt)
}

fn bindings_of(w: &WorkflowRecord) -> Result<RoleBindings, StorageError> {
    serde_json::from_value(w.config["bindings"].clone()).map_err(corrupt)
}

fn resume_text(state: &WorkflowState) -> Option<String> {
    if !matches!(state.status, WorkflowStatus::Waiting | WorkflowStatus::Paused) {
        return None;
    }
    Some(match &state.on_resume {
        None | Some(Resume::Continue) => "continue".into(),
        Some(Resume::Repair) => "send the agent another repair note".into(),
        Some(Resume::Start { step }) => format!("start {}", step.key()),
    })
}

fn wire(w: &WorkflowRecord) -> Result<WorkflowInfo, StorageError> {
    let state = state_of(w)?;
    let bindings = bindings_of(w)?;
    Ok(WorkflowInfo {
        task_id: w.task_id,
        definition: w.definition.clone(),
        version: w.version,
        status: w.status.clone(),
        stage: w.stage.clone(),
        step_key: w.step_key.clone(),
        reason: w.reason.clone(),
        resume_action: resume_text(&state),
        workspace_id: w.workspace_id,
        corrections: state.current.as_ref().map(Step::corrections).unwrap_or(0),
        max_corrections: MAX_CORRECTIONS,
        warnings: warnings_for(&bindings),
        bindings,
        started_by: w.started_by.clone(),
        created_at_ms: w.created_at_ms,
        updated_at_ms: w.updated_at_ms,
        ended_at_ms: w.ended_at_ms,
    })
}

/// The task's workflow, for `tasks.get`.
pub(crate) fn info(c: &Connection, task: Uuid) -> Result<Option<WorkflowInfo>, StorageError> {
    wf_store::find_workflow(c, task)?.as_ref().map(wire).transpose()
}

fn repair_note(n: u32, error: &str) -> String {
    format!(
        "Automatic repair note {n} of {max} from Switchyard: your last turn did not end with a usable \
         switchyard-report block ({error}). Continue the task as the task context describes and end your final message \
         with exactly one switchyard-report block.",
        max = wf::MAX_REPAIRS
    )
}

/// What a committed decision asks Core to do.
struct Followup {
    action: Action,
    assignment: Option<Uuid>,
    repair_error: Option<String>,
}

impl TaskService {
    fn workflow_lock(&self, task: Uuid) -> Arc<AsyncMutex<()>> {
        let mut locks = self.workflow_locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(locks.entry(task).or_default())
    }

    // --- settings ----------------------------------------------------------------

    pub async fn workflow_settings(&self, project_id: Uuid) -> Result<WorkflowSettingsInfo, ProtocolError> {
        let saved = self
            .storage
            .call(move |c| {
                switchyard_storage::projects::get(c, project_id)?;
                wf_store::settings(c, project_id)
            })
            .await
            .map_err(|e| match e {
                StorageError::NotFound(w) => {
                    ProtocolError::new(ErrorCode::ProjectNotFound, format!("{w} not found"))
                }
                other => errors::storage(other),
            })?;
        let (bindings, revision) = match saved {
            Some((b, rev)) => (serde_json::from_value(b).map_err(|e| errors::storage(corrupt(e)))?, rev),
            None => (default_bindings(), 0),
        };
        Ok(WorkflowSettingsInfo { project_id, warnings: warnings_for(&bindings), bindings, revision })
    }

    /// Checks every role's provider (installed, capable of the role) and model
    /// (offered by the installed CLI). Returns the warnings.
    async fn validate_bindings(&self, b: &RoleBindings) -> Result<Vec<String>, ProtocolError> {
        for which in Binding::ALL {
            let rb = binding_for(b, which);
            let provider = rb.provider.parse::<AgentProvider>().map_err(|_| {
                invalid_params(format!(
                    "{}: unknown provider {:?}; expected \"claude_code\" or \"codex\"",
                    which.as_str(),
                    rb.provider
                ))
            })?;
            let prefix = |e: ProtocolError| ProtocolError {
                message: format!("{}: {}", which.as_str(), e.message),
                ..e
            };
            self.check_provider(provider, which.role()).await.map_err(prefix)?;
            self.agents.validate_model(provider, rb.model.as_deref()).await.map_err(prefix)?;
        }
        Ok(warnings_for(b))
    }

    pub async fn set_workflow_settings(
        &self,
        project_id: Uuid,
        bindings: RoleBindings,
        actor: String,
    ) -> Result<WorkflowSettingsInfo, ProtocolError> {
        self.workflow_settings(project_id).await?;
        self.validate_bindings(&bindings).await?;
        let after =
            serde_json::to_value(&bindings).map_err(|e| errors::internal("cannot encode bindings", e))?;
        commit_checked(&self.storage, &self.bus, move |tx| {
            let before = wf_store::settings(tx, project_id)?.map(|(b, _)| b);
            let revision = wf_store::set_settings(tx, project_id, &after, now_ms())?;
            let ctx = switchyard_events::EventContext { project_id: Some(project_id), ..Default::default() };
            let d = draft(
                DomainEvent::WorkflowSettingsChanged {
                    project_id,
                    before,
                    after,
                    revision,
                    changed_by: actor,
                },
                &ctx,
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await?;
        self.workflow_settings(project_id).await
    }

    // --- queries and operator actions ----------------------------------------------

    pub async fn workflow(&self, task_id: Uuid) -> Result<Option<WorkflowInfo>, ProtocolError> {
        self.storage.call(move |c| info(c, task_id)).await.map_err(errors::storage)
    }

    async fn workflow_required(&self, task_id: Uuid) -> Result<WorkflowInfo, ProtocolError> {
        self.workflow(task_id)
            .await?
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, format!("task {task_id} has no workflow")))
    }

    /// Starts `engineering@1` on a ready task: snapshots the effective
    /// bindings, allocates its workspace and starts the investigation.
    pub async fn start_workflow(
        &self,
        p: WorkflowsStartParams,
        actor: String,
    ) -> Result<WorkflowInfo, ProtocolError> {
        let task_id = p.task_id;
        let task = self.storage.call(move |c| store::get_task(c, task_id)).await.map_err(|e| match e {
            StorageError::NotFound(w) => ProtocolError::new(ErrorCode::NotFound, format!("{w} not found")),
            other => errors::storage(other),
        })?;
        if task.status != TaskStatus::Ready {
            return Err(invalid_state(format!(
                "a workflow starts on a ready task; the task is {}",
                task.status
            )));
        }
        let settings = self.workflow_settings(task.project_id).await?;
        let bindings = apply_patch(settings.bindings, p.overrides);
        self.validate_bindings(&bindings).await?;
        let workspace = match p.workspace_id {
            Some(id) => {
                let w = self.workspaces.record(id).await?;
                if w.project_id != task.project_id {
                    return Err(invalid_params("the workspace belongs to another project"));
                }
                id
            }
            None => {
                let name = format!("task-{}", &task.id.simple().to_string()[..12]);
                self.workspaces.create(task.project_id, Some(name), None).await?.id
            }
        };
        let config = json!({ "bindings": bindings, "limits": {
            "max_corrections": MAX_CORRECTIONS, "max_repairs": wf::MAX_REPAIRS, "max_retries": wf::MAX_RETRIES } });
        let lock = self.workflow_lock(task_id);
        let _guard = lock.lock().await;
        let started_by = actor.clone();
        let created = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let t = check!(lookup(store::get_task(tx, task_id), ErrorCode::NotFound)?);
            if t.status != TaskStatus::Ready || store::open_assignment(tx, task_id)?.is_some() {
                return Ok(Err(invalid_state("the task changed meanwhile; it is no longer ready")));
            }
            let record = WorkflowRecord {
                task_id,
                project_id: t.project_id,
                definition: wf::DEFINITION.into(),
                version: wf::VERSION,
                config: config.clone(),
                workspace_id: Some(workspace),
                status: WorkflowStatus::Pending.as_str().into(),
                stage: None,
                step_key: None,
                reason: None,
                state: serde_json::to_value(WorkflowState::default()).map_err(corrupt)?,
                // Only events from now on concern this workflow.
                last_event_seq: switchyard_storage::events::last_seq(tx)?.unwrap_or(0),
                revision: 1,
                started_by: started_by.clone(),
                created_at_ms: now,
                updated_at_ms: now,
                ended_at_ms: None,
            };
            match wf_store::insert_workflow(tx, &record) {
                Ok(()) => {}
                Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
                Err(e) => return Err(e),
            }
            drafts.push(draft(
                DomainEvent::WorkflowStarted {
                    definition: wf::DEFINITION.into(),
                    version: wf::VERSION,
                    workspace_id: Some(workspace),
                    bindings: config["bindings"].clone(),
                    started_by: started_by.clone(),
                },
                &task_context(&t),
            )?);
            let followup = check!(decide_in_tx(tx, task_id, &Input::Start, &started_by, now, &mut drafts)?);
            Ok(Ok((followup, drafts)))
        })
        .await?;
        drop(_guard);
        if let Some(f) = created {
            self.perform(task_id, f);
        }
        self.workflow_required(task_id).await
    }

    pub async fn pause_workflow(&self, task_id: Uuid, actor: String) -> Result<WorkflowInfo, ProtocolError> {
        self.operator_input(task_id, Input::Pause, actor).await
    }

    pub async fn resume_workflow(&self, task_id: Uuid, actor: String) -> Result<WorkflowInfo, ProtocolError> {
        // An operator assignment must not be running underneath a resumed workflow.
        let open =
            self.storage.call(move |c| store::open_assignment(c, task_id)).await.map_err(errors::storage)?;
        if let Some(a) = open
            && a.origin != "workflow"
        {
            return Err(invalid_state(
                "an operator assignment is open on the task; finish or cancel it first",
            ));
        }
        self.unblock_for_resume(task_id).await?;
        self.operator_input(task_id, Input::Resume, actor).await
    }

    pub async fn cancel_workflow(
        &self,
        task_id: Uuid,
        reason: Option<String>,
        actor: String,
    ) -> Result<WorkflowInfo, ProtocolError> {
        let reason =
            reason.filter(|r| !r.trim().is_empty()).unwrap_or_else(|| format!("cancelled by {actor}"));
        self.operator_input(task_id, Input::Cancel { reason }, actor).await
    }

    /// A task an agent reported blocked goes back to work through the
    /// existing operator retry (the same transition the UI's Retry uses).
    async fn unblock_for_resume(&self, task_id: Uuid) -> Result<(), ProtocolError> {
        let status =
            self.storage.call(move |c| store::get_task(c, task_id)).await.map_err(errors::storage)?.status;
        if status == TaskStatus::Blocked {
            self.retry(task_id, Some("resumed by the workflow".into()), "workflow".into()).await?;
        }
        Ok(())
    }

    async fn operator_input(
        &self,
        task_id: Uuid,
        input: Input,
        actor: String,
    ) -> Result<WorkflowInfo, ProtocolError> {
        self.workflow_required(task_id).await?;
        self.apply_input(task_id, input, None, actor).await?;
        self.workflow_required(task_id).await
    }

    /// Decides on `input` and commits the result (with `seq` marked processed),
    /// then performs the follow-up. Serialized per task.
    async fn apply_input(
        &self,
        task_id: Uuid,
        input: Input,
        seq: Option<i64>,
        by: String,
    ) -> Result<(), ProtocolError> {
        let lock = self.workflow_lock(task_id);
        let _guard = lock.lock().await;
        let followup = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            if wf_store::find_workflow(tx, task_id)?.is_none() {
                return Ok(Ok((None, drafts)));
            }
            if let Some(seq) = seq
                && !wf_store::advance_workflow_seq(tx, task_id, seq)?
            {
                return Ok(Ok((None, drafts)));
            }
            let f = check!(decide_in_tx(tx, task_id, &input, &by, now, &mut drafts)?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(_guard);
        if let Some(f) = followup {
            self.perform(task_id, f);
        }
        Ok(())
    }

    /// Performs a committed decision's follow-up in the background (a session
    /// start can take seconds; the driver keeps following the journal).
    fn perform(&self, task_id: Uuid, f: Followup) {
        let svc = self.clone();
        tokio::spawn(async move {
            match f.action {
                Action::Start(step) => {
                    if let Some(id) = f.assignment
                        && let Err(e) = svc.start_assignment(id).await
                    {
                        // `start_assignment` already ended the assignment as failed;
                        // that event drives the retry policy.
                        tracing::warn!(%task_id, step = %step.key(), ?e, "a workflow step could not start");
                    }
                }
                Action::Repair { n } => {
                    let note = repair_note(n, f.repair_error.as_deref().unwrap_or("no report"));
                    if let Err(e) = svc.note(task_id, note, "workflow".into()).await {
                        tracing::warn!(%task_id, ?e, "cannot send a workflow repair note");
                    }
                }
                Action::CancelAssignment => {
                    let open = svc.storage.call(move |c| store::open_assignment(c, task_id)).await;
                    if let Ok(Some(a)) = open
                        && a.origin == "workflow"
                        && let Err(e) = svc
                            .cancel_assignment(
                                a.id,
                                Some("the workflow was cancelled".into()),
                                "workflow".into(),
                            )
                            .await
                    {
                        tracing::warn!(%task_id, ?e, "cannot cancel the workflow's assignment");
                    }
                }
                Action::None => {}
            }
        });
    }

    // --- following the journal ---------------------------------------------------

    /// Turns one journal event into a workflow input, if it concerns a workflow.
    async fn react_workflow(&self, e: &EventEnvelope) {
        let Some(task_id) = e.context.task_id else { return };
        let seq = e.seq;
        let found = self.storage.call(move |c| wf_store::find_workflow(c, task_id)).await;
        let w = match found {
            Ok(Some(w))
                if seq > w.last_event_seq
                    && !WorkflowStatus::parse(&w.status).is_none_or(|s| s.is_final()) =>
            {
                w
            }
            Ok(_) => return,
            Err(err) => {
                tracing::error!(%task_id, ?err, "workflow driver cannot read the workflow");
                return;
            }
        };
        let Ok(event) = e.decode() else { return };
        let input = match self.input_for(&event).await {
            Ok(i) => i,
            Err(err) => {
                tracing::error!(%task_id, ?err, "workflow driver cannot interpret an event");
                return;
            }
        };
        let Some(input) = input else { return };
        if let Err(err) = self.apply_input(w.task_id, input, Some(seq), "workflow".into()).await {
            tracing::error!(%task_id, ?err, "workflow driver cannot apply an event");
        }
    }

    async fn input_for(&self, event: &DomainEvent) -> Result<Option<Input>, ProtocolError> {
        let step_of = |id: Uuid| async move {
            self.storage
                .call(move |c| {
                    let a = store::get_assignment(c, id)?;
                    let t = store::get_task(c, a.task_id)?;
                    Ok((a, t))
                })
                .await
                .map_err(errors::storage)
        };
        Ok(match event {
            DomainEvent::TaskAssignmentEnded { assignment_id, status, outcome } => {
                let (a, t) = step_of(*assignment_id).await?;
                let Some(step) = a.step_key else { return Ok(None) };
                let outcome = outcome.clone().unwrap_or_default();
                match status.as_str() {
                    "reported" => {
                        let summary = t.status_reason.as_deref().map(|r| r.trim_start_matches("agent: "));
                        match Outcome::from_assignment(&outcome, summary) {
                            Some(o) => Some(Input::Reported { step, outcome: o }),
                            None => Some(Input::Ended { step, cancelled: false, reason: outcome }),
                        }
                    }
                    "cancelled" => Some(Input::Ended { step, cancelled: true, reason: outcome }),
                    _ => Some(Input::Ended { step, cancelled: false, reason: outcome }),
                }
            }
            DomainEvent::TaskMessageRecorded { kind, sender, assignment_id: Some(id), summary, .. } => {
                let unusable = kind == "malformed_report" && sender == "core";
                let question = kind == "info_request" && sender == "agent";
                if !unusable && !question {
                    return Ok(None);
                }
                let (a, _) = step_of(*id).await?;
                let Some(step) = a.step_key else { return Ok(None) };
                if a.status != AssignmentStatus::Active {
                    return Ok(None);
                }
                let summary = summary.clone().unwrap_or_else(|| "no details".into());
                if unusable {
                    Some(Input::Unusable { step, error: summary })
                } else {
                    Some(Input::Question { step, summary })
                }
            }
            DomainEvent::TaskStatusChanged { to, .. }
                if matches!(to.as_str(), "cancelled" | "failed" | "completed") =>
            {
                Some(Input::TaskClosed { status: to.clone() })
            }
            _ => None,
        })
    }

    /// At startup: a step whose session was running when Core stopped was
    /// interrupted. The workflow waits for the operator (agents are never
    /// restarted automatically after a restart); a step that was created but
    /// never started is started now.
    async fn recover_workflows(&self) {
        let active = match self.storage.call(|c| wf_store::active_workflows(c)).await {
            Ok(a) => a,
            Err(err) => {
                tracing::error!(?err, "cannot read workflows to recover");
                return;
            }
        };
        for w in active {
            let Ok(state) = state_of(&w) else { continue };
            if state.status != WorkflowStatus::Running {
                continue;
            }
            let Some(step) = state.current.clone() else { continue };
            let (task_id, key) = (w.task_id, step.key());
            let k = key.clone();
            let found = self
                .storage
                .call(move |c| {
                    Ok(store::assignments_for_task(c, task_id)?
                        .into_iter()
                        .find(|a| a.step_key.as_deref() == Some(&k)))
                })
                .await;
            match found {
                Ok(Some(a)) if a.status == AssignmentStatus::Pending => {
                    tracing::info!(%task_id, step = %key, "starting a workflow step created before the restart");
                    self.perform(
                        task_id,
                        Followup { action: Action::Start(step), assignment: Some(a.id), repair_error: None },
                    );
                }
                Ok(Some(a)) if a.status != AssignmentStatus::Reported => {
                    // Active (its session did not survive) or ended without the
                    // workflow having processed it: the operator decides.
                    tracing::info!(%task_id, step = %key, "a workflow step was interrupted by a restart");
                    if let Err(err) =
                        self.apply_input(task_id, Input::Interrupted { step: key }, None, "core".into()).await
                    {
                        tracing::error!(%task_id, ?err, "cannot record an interrupted workflow step");
                    }
                }
                // Reported: the journal replay below advances the workflow.
                Ok(_) => {}
                Err(err) => tracing::error!(%task_id, ?err, "cannot read a workflow step"),
            }
        }
    }
}

/// Decides on `input` for the task's workflow inside `tx`: writes the new
/// state, its `workflow.*` events and (for a start) the step's assignment.
/// `None` when nothing changed.
fn decide_in_tx(
    tx: &Connection,
    task_id: Uuid,
    input: &Input,
    by: &str,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> super::Checked<Option<Followup>> {
    let w = wf_store::get_workflow(tx, task_id)?;
    let state = state_of(&w)?;
    let mut d = match wf::decide(&state, input) {
        Ok(d) => d,
        Err(e) => return Ok(Err(invalid_state(e.to_string()))),
    };
    if d.state == state && d.action == Action::None && d.changes.is_empty() {
        return Ok(Ok(None));
    }
    let t = store::get_task(tx, task_id)?;
    let ctx = task_context(&t);
    let bindings = bindings_of(&w)?;
    let mut assignment = None;
    if let Action::Start(step) = d.action.clone() {
        let b = binding_for(&bindings, step.binding);
        let new = NewAssignment {
            task_id,
            role: step.role(),
            provider: b.provider.clone(),
            model: b.model.clone(),
            workspace_id: w
                .workspace_id
                .ok_or_else(|| StorageError::Corrupt("workflow without a workspace".into()))?,
            workspace_project: w.project_id,
            origin: "workflow",
            step_key: Some(step.key()),
            by: "workflow".into(),
        };
        // A savepoint keeps a refused step from leaving half its rows behind.
        tx.execute_batch("SAVEPOINT workflow_step")?;
        let mark = drafts.len();
        match create_assignment(tx, &new, now, drafts)? {
            Ok(id) => {
                tx.execute_batch("RELEASE workflow_step")?;
                assignment = Some(id);
                for c in &mut d.changes {
                    if let Change::StageChanged(s) = c
                        && *s == step
                    {
                        drafts.push(draft(
                            DomainEvent::WorkflowStageChanged {
                                stage: step.stage.as_str().into(),
                                step_key: step.key(),
                                assignment_id: id,
                                role: step.role().as_str().into(),
                                provider: b.provider.clone(),
                                model: b.model.clone(),
                            },
                            &ctx,
                        )?);
                    }
                }
                d.changes.retain(|c| !matches!(c, Change::StageChanged(_)));
            }
            Err(e) => {
                tx.execute_batch("ROLLBACK TO workflow_step; RELEASE workflow_step")?;
                drafts.truncate(mark);
                // Never fail silently: wait, saying why, and start it on resume.
                let reason =
                    format!("{} could not be started: {}. Resume to try again.", step.key(), e.message);
                d.state.status = WorkflowStatus::Waiting;
                d.state.reason = Some(reason.clone());
                d.state.on_resume = Some(Resume::Start { step: step.clone() });
                d.changes.retain(|c| !matches!(c, Change::StageChanged(_)));
                d.changes.push(Change::Waiting(reason));
                d.action = Action::None;
            }
        }
    }
    let step_key = d.state.current.as_ref().map(Step::key);
    for c in &d.changes {
        let event = match c {
            Change::Started | Change::StageChanged(_) => continue,
            Change::Waiting(reason) => {
                DomainEvent::WorkflowWaiting { step_key: step_key.clone(), reason: reason.clone() }
            }
            Change::Paused => DomainEvent::WorkflowPaused { by: by.into() },
            Change::Resumed => DomainEvent::WorkflowResumed { by: by.into() },
            Change::Completed(reason) => DomainEvent::WorkflowCompleted { reason: reason.clone() },
            Change::Failed(reason) => DomainEvent::WorkflowFailed { reason: reason.clone() },
            Change::Cancelled(reason) => DomainEvent::WorkflowCancelled { reason: reason.clone() },
        };
        drafts.push(draft(event, &ctx)?);
    }
    let repair_error = match input {
        Input::Unusable { error, .. } => Some(error.clone()),
        _ => None,
    };
    let state_json = serde_json::to_value(&d.state).map_err(corrupt)?;
    let update = WorkflowUpdate {
        status: d.state.status.as_str(),
        stage: d.state.stage.map(wf::Stage::as_str),
        step_key: step_key.as_deref(),
        reason: d.state.reason.as_deref(),
        state: &state_json,
        workspace_id: None,
        ended: d.state.status.is_final(),
    };
    wf_store::update_workflow(tx, task_id, w.revision, &update, now)?;
    Ok(Ok(Some(Followup { action: d.action, assignment, repair_error })))
}

/// Follows the journal for the workflows of all tasks. Started after the
/// task reactor; first settles what a previous Core left running.
pub(crate) fn spawn_workflow_driver(
    svc: TaskService,
    mut shutdown: watch::Receiver<Option<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = svc
            .storage
            .call(|c| {
                let lowest = wf_store::active_workflows(c)?.iter().map(|w| w.last_event_seq).min();
                Ok(match lowest {
                    Some(seq) => seq,
                    None => switchyard_storage::events::last_seq(c)?.unwrap_or(0),
                })
            })
            .await;
        let start = match start {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(
                    ?err,
                    "workflow driver cannot read its starting point; workflows will not advance"
                );
                return;
            }
        };
        svc.recover_workflows().await;
        let filter = EventFilter { event_types: WATCHED.map(str::to_owned).to_vec(), ..Default::default() };
        let mut sub = svc.bus.subscribe(Some(start), filter);
        loop {
            tokio::select! {
                () = wait_shutdown(&mut shutdown) => break,
                next = sub.next() => match next {
                    Ok(event) => svc.react_workflow(&event).await,
                    Err(RecvError::Closed) => break,
                    Err(err) => {
                        tracing::error!(?err, "workflow driver cannot read the journal; retrying");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrections_default_to_the_implementation_binding_and_overrides_apply() {
        let b = default_bindings();
        assert_eq!(binding_for(&b, Binding::Correct), &b.implement);
        let patched = apply_patch(
            b.clone(),
            RoleBindingsPatch {
                review: Some(RoleBinding { provider: "codex".into(), model: Some("m".into()) }),
                ..Default::default()
            },
        );
        assert_eq!(patched.review.model.as_deref(), Some("m"));
        assert_eq!(patched.investigate, b.investigate);
        assert!(!warnings_for(&b).is_empty() || b.implement.provider != b.review.provider);
        assert!(warnings_for(&patched).iter().any(|w| w.contains("both use")));
    }

    #[test]
    fn a_provider_without_permission_routing_cannot_take_a_role() {
        let mut caps = switchyard_agents::capabilities(AgentProvider::ClaudeCode);
        assert!(super::super::assign::role_capabilities(switchyard_tasks::Role::Review, &caps).is_ok());
        caps.permission_requests = switchyard_agents::Support::Unsupported;
        let err = super::super::assign::role_capabilities(switchyard_tasks::Role::Review, &caps).unwrap_err();
        assert!(err.contains("permission requests"), "{err}");
    }
}
