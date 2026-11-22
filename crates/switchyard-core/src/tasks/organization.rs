//! Agent organizations (milestone 8.3, ADRs 0035–0036): the driver around
//! the pure [`switchyard_tasks::organization::decide`].
//!
//! * **Organization** and **task graph** live in the pure state (agents,
//!   organization tasks) and in M6 tasks (one per organization task, created
//!   through the proposal path and decided by Core within the limits).
//! * **Runtime**: specialists work through ordinary M6 assignments (policies,
//!   approvals, read-only reviews, reports); supervisors run their own
//!   read-only sessions, resumed at each checkpoint. One session at a time
//!   per organization workspace: Core alternates work and supervision.
//! * **Supervision**: every work turn ends in a checkpoint for the owner's
//!   supervisor, with Core-verified evidence (changed files from Git, files
//!   outside the scope). During a turn Core watches tool activity; work that
//!   starts to leave its scope is interrupted at once and its supervisor
//!   decides. Instructions reach a specialist in its next turn's context,
//!   never inside a running turn (steering is not verified for either CLI).
//!
//! Every decision is committed (state, events, subtasks, context, the next
//! assignment) before anything starts. Agents are never restarted
//! automatically after a Core restart.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use switchyard_agents::profiles::{self, Estimate, Override, Requirement};
use switchyard_agents::{AgentProvider, SessionAccess};
use switchyard_events::{DomainEvent, EventDraft, EventEnvelope, EventFilter, RecvError, new_id, now_ms};
use switchyard_protocol::methods::WorkflowsStartParams;
use switchyard_protocol::model::{
    OrgActivity, OrgAgentInfo, OrgContextEntry, OrgProfileInfo, OrgSettingsInfo, OrgTaskInfo, OrgUsage,
    OrganizationInfo, WorkflowInfo,
};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::organizations::{self as org_store, OrgRecord};
use switchyard_storage::tasks::{self as store, ProposalRecord, TaskRecord};
use switchyard_storage::workflows::{self as wf_store, WorkflowRecord};
use switchyard_storage::{Connection, StorageError, sessions};
use switchyard_tasks::organization::{
    self as org, Action, Activity, Agent, AgentKind, AgentStatus, Change, Checkpoint, CheckpointItem,
    ContextEntry, ContextKind, ContextStatus, Directive, Evidence, Input, Limits, Observation, OrgState,
    OrgStatus, OrgTask, Selected, TaskState, WorkKind, WorkerOutcome,
};
use switchyard_tasks::workflow::{Stage, WorkflowState, WorkflowStatus};
use switchyard_tasks::{AssignmentStatus, MessageKind, ProposalStatus, ReviewResult, Role, TaskStatus};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::assign::{NewAssignment, create_assignment};
use super::{
    Checked, TaskService, add_dependency_in_tx, check, insert_task_in_tx, invalid_params, invalid_state,
    new_message, record_message, refresh_readiness, set_status, task_context,
};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};
use crate::state::wait_shutdown;

const WATCHED: [&str; 9] = [
    "agent.turn_completed",
    "agent.exited",
    "agent.failed",
    "agent.tool_started",
    "agent.tool_failed",
    "agent.permission_requested",
    "task.assignment_ended",
    "task.message_recorded",
    "task.status_changed",
];
/// Failed tool operations in one work turn that make Core interrupt it.
const MAX_TOOL_FAILURES: u32 = 3;
const ACTOR: &str = "organization";
/// Why Core closes a step it handed to the supervisor (a question, an unusable turn).
const HANDED_OFF: &str = "handed to its supervisor";

/// The project's organization settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct OrgSettings {
    pub limits: Limits,
    pub estimates: Vec<Estimate>,
    pub overrides: Vec<Override>,
}

fn corrupt(e: serde_json::Error) -> StorageError {
    StorageError::Corrupt(format!("organization state: {e}"))
}

fn state_of(r: &OrgRecord) -> Result<OrgState, StorageError> {
    serde_json::from_value(r.state.clone()).map_err(corrupt)
}

fn entries_of(c: &Connection, org: Uuid) -> Result<Vec<ContextEntry>, StorageError> {
    Ok(org_store::context(c, org)?
        .into_iter()
        .filter_map(|r| {
            Some(ContextEntry {
                seq: r.seq,
                kind: ContextKind::parse(&r.kind)?,
                status: ContextStatus::parse(&r.status)?,
                text: r.text,
                task: r.task_key,
                source: r.source,
                supersedes: r.supersedes,
            })
        })
        .collect())
}

pub(crate) fn settings_of(c: &Connection, project: Uuid) -> Result<(OrgSettings, i64), StorageError> {
    Ok(match org_store::settings(c, project)? {
        Some((v, rev)) => (serde_json::from_value(v).map_err(corrupt)?, rev),
        None => (OrgSettings::default(), 0),
    })
}

/// The organization brief of a specialist's task context, if `task` is work
/// in an organization.
pub(crate) fn brief_for(c: &Connection, task: &TaskRecord) -> Result<Option<org::OrgBrief>, StorageError> {
    let Some(rec) = org_store::for_task(c, task.id)? else { return Ok(None) };
    if rec.task_id == task.id {
        return Ok(None);
    }
    let state = state_of(&rec)?;
    let Some(t) = state.task_by_id(task.id) else { return Ok(None) };
    let entries = entries_of(c, rec.task_id)?;
    Ok(org::worker_brief(&state, &t.key, &entries, &t.current_instructions))
}

fn item_label(i: &CheckpointItem) -> String {
    match i {
        CheckpointItem::Plan => "plan".into(),
        CheckpointItem::Reported { task, .. } => format!("review {task}"),
        CheckpointItem::Observed { task, .. } => format!("observation on {task}"),
        CheckpointItem::Escalated { from, .. } => format!("escalation from {from}"),
        CheckpointItem::Answer { .. } => "your answer".into(),
        CheckpointItem::Rejected { .. } => "rejected directive".into(),
        CheckpointItem::Instruction { from, .. } => format!("instruction from {from}"),
        CheckpointItem::ChildrenFinished => "scope finished".into(),
    }
}

/// What Core does after committing a decision.
struct Followup {
    action: Action,
    assignment: Option<Uuid>,
    /// The supervisor session to stop before anything else starts.
    stop_session: Option<Uuid>,
}

/// In-memory counters of failed tool operations per running assignment.
#[derive(Clone, Default)]
pub(crate) struct Monitor {
    failures: Arc<Mutex<HashMap<Uuid, u32>>>,
}

impl Monitor {
    fn count(&self, assignment: Uuid) -> u32 {
        let mut m = self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = m.entry(assignment).or_insert(0);
        *n += 1;
        *n
    }
}

/// Paths a tool request would change, relative to the workspace when inside it.
fn changed_paths(tool: &str, input: &Value, workspace: &str) -> Vec<String> {
    let rel = |p: &str| -> String {
        let base = workspace.trim_end_matches('/');
        p.strip_prefix(&format!("{base}/")).map(str::to_owned).unwrap_or_else(|| p.to_owned())
    };
    let mut out = Vec::new();
    match tool {
        "Write" | "Edit" => {
            if let Some(p) = input.get("file_path").and_then(Value::as_str) {
                out.push(rel(p));
            }
        }
        "NotebookEdit" => {
            if let Some(p) = input.get("notebook_path").and_then(Value::as_str) {
                out.push(rel(p));
            }
        }
        "apply_patch" => {
            for c in input.get("changes").and_then(Value::as_array).into_iter().flatten() {
                if let Some(p) = c.get("path").and_then(Value::as_str) {
                    out.push(rel(p));
                }
            }
        }
        _ => {}
    }
    out
}

impl TaskService {
    // --- settings ------------------------------------------------------------------

    async fn catalogs(&self) -> Vec<switchyard_agents::ModelCatalog> {
        let mut out = Vec::new();
        for p in AgentProvider::ALL {
            let a = self.agents.availability(*p, false).await;
            if a.installed && a.error.is_none() {
                out.push(self.agents.models(*p, false).await);
            }
        }
        out
    }

    pub async fn org_settings(&self, project_id: Uuid) -> Result<OrgSettingsInfo, ProtocolError> {
        let (s, revision) =
            self.storage.call(move |c| settings_of(c, project_id)).await.map_err(errors::storage)?;
        let catalogs = self.catalogs().await;
        let profiles = profiles::profiles(&catalogs, &s.estimates)
            .into_iter()
            .map(|p| OrgProfileInfo {
                provider: p.provider.as_str().into(),
                model: p.model.clone(),
                display: p.display.clone(),
                summary: p.describe(),
                runtime: serde_json::to_value(&p.runtime).unwrap_or(Value::Null),
                skills: serde_json::to_value(&p.skills).unwrap_or(Value::Null),
            })
            .collect();
        Ok(OrgSettingsInfo {
            project_id,
            limits: serde_json::to_value(&s.limits).unwrap_or(Value::Null),
            estimates: serde_json::to_value(&s.estimates).unwrap_or(Value::Null),
            overrides: serde_json::to_value(&s.overrides).unwrap_or(Value::Null),
            revision,
            profiles,
            skills: profiles::SKILLS.iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    pub async fn set_org_settings(
        &self,
        project_id: Uuid,
        limits: Option<Value>,
        estimates: Option<Value>,
        overrides: Option<Value>,
    ) -> Result<OrgSettingsInfo, ProtocolError> {
        let bad = |what: &str, e: serde_json::Error| invalid_params(format!("{what}: {e}"));
        let limits: Option<Limits> =
            limits.map(serde_json::from_value).transpose().map_err(|e| bad("limits", e))?;
        if let Some(l) = &limits {
            l.validate().map_err(invalid_params)?;
        }
        let estimates: Option<Vec<Estimate>> =
            estimates.map(serde_json::from_value).transpose().map_err(|e| bad("estimates", e))?;
        for e in estimates.iter().flatten() {
            if AgentProvider::ALL.iter().all(|p| p.as_str() != e.provider) {
                return Err(invalid_params(format!("unknown provider {}", e.provider)));
            }
            if let Some(k) = e.skills.keys().find(|k| !profiles::SKILLS.contains(&k.as_str())) {
                return Err(invalid_params(format!("unknown skill {k}")));
            }
            if e.relative_cost.is_some_and(|c| !(1..=5).contains(&c))
                || e.relative_speed.is_some_and(|c| !(1..=5).contains(&c))
            {
                return Err(invalid_params("relative cost and speed are 1 to 5"));
            }
        }
        let overrides: Option<Vec<Override>> =
            overrides.map(serde_json::from_value).transpose().map_err(|e| bad("overrides", e))?;
        for o in overrides.iter().flatten() {
            if !profiles::SKILLS.contains(&o.skill.as_str()) {
                return Err(invalid_params(format!("unknown skill {}", o.skill)));
            }
            let Some(p) = AgentProvider::ALL.iter().find(|p| p.as_str() == o.provider) else {
                return Err(invalid_params(format!("unknown provider {}", o.provider)));
            };
            self.agents.models(*p, false).await.validate(o.model.as_deref()).map_err(invalid_params)?;
        }
        let new_limits = limits.clone();
        commit_checked(&self.storage, &self.bus, move |tx| {
            let (mut s, _) = settings_of(tx, project_id)?;
            let before = org_store::settings(tx, project_id)?.map(|(v, _)| v);
            if let Some(l) = limits {
                s.limits = l;
            }
            if let Some(e) = estimates {
                s.estimates = e;
            }
            if let Some(o) = overrides {
                s.overrides = o;
            }
            let after = serde_json::to_value(&s).map_err(corrupt)?;
            let revision = org_store::put_settings(tx, project_id, &after, now_ms())?;
            let ctx = switchyard_events::EventContext { project_id: Some(project_id), ..Default::default() };
            let drafts = vec![draft(
                DomainEvent::OrganizationSettingsChanged { project_id, before, after, revision },
                &ctx,
            )?];
            Ok(Ok(((), drafts)))
        })
        .await?;
        // Running organizations of the project get the new limits.
        if let Some(l) = new_limits {
            let active = self.storage.call(|c| org_store::active(c)).await.map_err(errors::storage)?;
            for o in active.into_iter().filter(|o| o.project_id == project_id) {
                self.apply_org(o.task_id, Input::SetLimits(l.clone()), None).await?;
            }
        }
        self.org_settings(project_id).await
    }

    // --- start ---------------------------------------------------------------------

    /// Starts an organization for a ready task (`workflows.start` with
    /// `definition: organization`).
    pub(super) async fn start_organization(
        &self,
        task: TaskRecord,
        p: WorkflowsStartParams,
        actor: String,
    ) -> Result<WorkflowInfo, ProtocolError> {
        let project_id = task.project_id;
        let (settings, _) =
            self.storage.call(move |c| settings_of(c, project_id)).await.map_err(errors::storage)?;
        let catalogs = self.catalogs().await;
        let lead = match &p.lead {
            Some(b) => {
                let provider: AgentProvider = b
                    .provider
                    .parse()
                    .map_err(|_| invalid_params(format!("unknown provider {}", b.provider)))?;
                self.check_provider(provider, Role::Review).await?;
                if !switchyard_agents::capabilities(provider).resume_session.is_supported() {
                    return Err(invalid_params(
                        "the lead supervisor's provider must be able to resume sessions",
                    ));
                }
                self.agents
                    .models(provider, false)
                    .await
                    .validate(b.model.as_deref())
                    .map_err(invalid_params)?;
                Selected {
                    provider: b.provider.clone(),
                    model: b.model.clone(),
                    rationale: "chosen by you when starting".into(),
                }
            }
            None => {
                let req =
                    Requirement { skills: vec!["reasoning".into()], supervisor: true, ..Default::default() };
                let c = profiles::select(
                    &profiles::profiles(&catalogs, &settings.estimates),
                    &req,
                    &settings.overrides,
                )
                .map_err(|e| ProtocolError::new(ErrorCode::AgentUnavailable, e))?;
                Selected { provider: c.provider.as_str().into(), model: c.model, rationale: c.rationale }
            }
        };
        let workspace = match p.workspace_id {
            Some(id) => {
                let w = self.workspaces.record(id).await?;
                if w.project_id != project_id {
                    return Err(invalid_params("the workspace belongs to another project"));
                }
                w
            }
            None => {
                let name = format!("task-{}", &task.id.simple().to_string()[..12]);
                let id = self.workspaces.create(project_id, Some(name), None).await?.id;
                self.workspaces.record(id).await?
            }
        };
        let workspace_id = workspace.id;
        let base_commit = workspace.base_commit.clone();
        let limits = settings.limits.clone();
        let task_id = task.id;
        let lock = self.workflow_lock(task_id);
        let _guard = lock.lock().await;
        let started_by = actor.clone();
        let followup = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let t = store::get_task(tx, task_id)?;
            if t.status != TaskStatus::Ready || store::open_assignment(tx, task_id)?.is_some() {
                return Ok(Err(invalid_state("the task changed meanwhile; it is no longer ready")));
            }
            let lead_agent = Agent {
                id: new_id(),
                key: org::LEAD.into(),
                title: "Lead supervisor".into(),
                kind: AgentKind::Supervisor,
                reports_to: None,
                responsibility: format!("Deliver the objective: {}", t.title),
                scope: Vec::new(),
                capabilities: vec!["reasoning".into()],
                provider: lead.provider.clone(),
                model: lead.model.clone(),
                selection: lead.rationale.clone(),
                status: AgentStatus::Idle,
                created_by: started_by.clone(),
                depth: 1,
            };
            let objective = OrgTask {
                key: org::ROOT.into(),
                task_id: Some(task_id),
                title: t.title.clone(),
                description: t.description.clone(),
                work: WorkKind::Coordination,
                parent: None,
                owner: org::LEAD.into(),
                scope: Vec::new(),
                acceptance: t.acceptance_criteria.clone(),
                depends_on: Vec::new(),
                state: TaskState::Planning,
                corrections: 0,
                attempts: 0,
                instructions: Vec::new(),
                current_instructions: Vec::new(),
                summary: None,
                created_by: started_by.clone(),
            };
            let empty = OrgState {
                status: OrgStatus::Running,
                reason: None,
                limits: limits.clone(),
                agents: Vec::new(),
                tasks: Vec::new(),
                queue: Vec::new(),
                activity: Activity::Idle,
                repairs: 0,
                supervision_turns: 0,
                started_at_ms: now,
                cost_usd: 0.0,
                tokens: 0,
                question_from: None,
                final_summary: None,
                sessions: BTreeMap::new(),
                turn_after_seq: 0,
                dead_ends_reported: Vec::new(),
            };
            let workflow = WorkflowRecord {
                task_id,
                project_id: t.project_id,
                definition: org::DEFINITION.into(),
                version: org::VERSION,
                config: json!({ "bindings": super::workflow::default_bindings(), "lead": lead, "limits": limits }),
                workspace_id: Some(workspace_id),
                status: WorkflowStatus::Running.as_str().into(),
                stage: None,
                step_key: None,
                reason: Some("the organization is working on the objective".into()),
                state: serde_json::to_value(WorkflowState { status: WorkflowStatus::Running, ..Default::default() })
                    .map_err(corrupt)?,
                last_event_seq: switchyard_storage::events::last_seq(tx)?.unwrap_or(0),
                revision: 1,
                started_by: started_by.clone(),
                created_at_ms: now,
                updated_at_ms: now,
                ended_at_ms: None,
                integration_id: None,
            };
            match wf_store::insert_workflow(tx, &workflow) {
                Ok(()) => {}
                Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
                Err(e) => return Err(e),
            }
            let record = OrgRecord {
                task_id,
                project_id: t.project_id,
                workspace_id,
                status: "running".into(),
                reason: None,
                state: serde_json::to_value(&empty).map_err(corrupt)?,
                base_commit: base_commit.clone(),
                last_event_seq: workflow.last_event_seq,
                revision: 1,
                started_by: started_by.clone(),
                created_at_ms: now,
                updated_at_ms: now,
                ended_at_ms: None,
            };
            org_store::insert(tx, &record)?;
            let ctx = task_context(&t);
            drafts.push(draft(
                DomainEvent::WorkflowStarted {
                    definition: org::DEFINITION.into(),
                    version: org::VERSION,
                    workspace_id: Some(workspace_id),
                    bindings: workflow.config["bindings"].clone(),
                    started_by: started_by.clone(),
                },
                &ctx,
            )?);
            drafts.push(draft(
                DomainEvent::OrganizationStarted {
                    lead: serde_json::to_value(&lead_agent).map_err(corrupt)?,
                    limits: serde_json::to_value(&limits).map_err(corrupt)?,
                    workspace_id,
                    started_by: started_by.clone(),
                },
                &ctx,
            )?);
            check!(set_status(
                tx,
                task_id,
                TaskStatus::InProgress,
                Some("the agent organization is working on it".into()),
                None,
                None,
                now,
                &mut drafts
            )?);
            let input = Input::Start { objective: Box::new(objective), lead: Box::new(lead_agent), now_ms: now };
            let f = check!(decide_in_tx(tx, task_id, &input, None, now, &mut drafts)?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(_guard);
        if let Some(f) = followup {
            self.perform_org(task_id, f);
        }
        self.workflow_required(task_id).await
    }

    // --- decisions -----------------------------------------------------------------

    /// Decides on `input` and commits it (with `seq` marked processed), then
    /// performs the follow-up. Serialized per organization.
    pub(super) async fn apply_org(
        &self,
        task_id: Uuid,
        input: Input,
        seq: Option<i64>,
    ) -> Result<(), ProtocolError> {
        let lock = self.workflow_lock(task_id);
        let guard = lock.lock().await;
        let f = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            if org_store::find(tx, task_id)?.is_none() {
                return Ok(Ok((None, drafts)));
            }
            if let Some(seq) = seq
                && !org_store::advance_seq(tx, task_id, seq)?
            {
                return Ok(Ok((None, drafts)));
            }
            let f = check!(decide_in_tx(tx, task_id, &input, seq, now_ms(), &mut drafts)?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(guard);
        if let Some(f) = f {
            self.perform_org(task_id, f);
        }
        Ok(())
    }

    fn perform_org(&self, task_id: Uuid, f: Followup) {
        let svc = self.clone();
        tokio::spawn(async move {
            if let Some(s) = f.stop_session {
                svc.stop_session(s).await;
            }
            match f.action {
                Action::None => {}
                Action::Supervise { checkpoint } => {
                    if let Err(e) = svc.run_supervision(task_id, checkpoint).await {
                        tracing::warn!(%task_id, ?e, "a supervision turn could not start");
                        let _ = svc
                            .apply_org(
                                task_id,
                                Input::Interrupted {
                                    reason: format!("the supervision turn could not start: {}", e.message),
                                },
                                None,
                            )
                            .await;
                    }
                }
                Action::Work { task, .. } => {
                    if let Some(id) = f.assignment
                        && let Err(e) = svc.start_assignment(id).await
                    {
                        // `start_assignment` ended the assignment as failed; that event
                        // reaches the organization as a failed turn.
                        tracing::warn!(%task_id, %task, ?e, "a work turn could not start");
                    }
                }
                Action::Interrupt { task } => {
                    svc.interrupt_work(
                        task_id,
                        &task,
                        "Switchyard interrupted the turn so its supervisor can decide",
                    )
                    .await
                }
                Action::Stop => {
                    let rec = svc.storage.call(move |c| org_store::get(c, task_id)).await;
                    if let Ok(rec) = rec
                        && let Ok(state) = state_of(&rec)
                    {
                        for t in state.tasks.iter().filter_map(|t| t.task_id) {
                            if let Ok(Some(a)) = svc.storage.call(move |c| store::open_assignment(c, t)).await
                            {
                                let _ = svc
                                    .cancel_assignment(
                                        a.id,
                                        Some("the organization was cancelled".into()),
                                        ACTOR.into(),
                                    )
                                    .await;
                            }
                        }
                        for s in state.sessions.values() {
                            svc.stop_session(*s).await;
                        }
                    }
                }
                Action::Deliver { .. } => {
                    // The workflow is in its delivery stage now: M8.2 takes over.
                    svc.perform(
                        task_id,
                        super::workflow::Followup {
                            action: switchyard_tasks::workflow::Action::Prepare,
                            assignment: None,
                            repair_error: None,
                            by: None,
                        },
                    );
                }
            }
        });
    }

    async fn interrupt_work(&self, org_task: Uuid, key: &str, reason: &str) {
        let key = key.to_owned();
        let found = self
            .storage
            .call(move |c| {
                let rec = org_store::get(c, org_task)?;
                let state = state_of(&rec)?;
                Ok(match state.task(&key).and_then(|t| t.task_id) {
                    Some(t) => store::open_assignment(c, t)?,
                    None => None,
                })
            })
            .await;
        if let Ok(Some(a)) = found
            && let Err(e) = self.cancel_assignment(a.id, Some(reason.into()), ACTOR.into()).await
        {
            tracing::warn!(%org_task, ?e, "cannot interrupt a work turn");
        }
    }

    // --- supervision ---------------------------------------------------------------

    async fn run_supervision(&self, task_id: Uuid, checkpoint: Checkpoint) -> Result<(), ProtocolError> {
        let (rec, state, entries) = self
            .storage
            .call(move |c| {
                let rec = org_store::get(c, task_id)?;
                let state = state_of(&rec)?;
                let entries = entries_of(c, task_id)?;
                Ok((rec, state, entries))
            })
            .await
            .map_err(errors::storage)?;
        let Activity::Supervising { checkpoint: current } = &state.activity else { return Ok(()) };
        if *current != checkpoint {
            return Ok(());
        }
        let agent = state
            .agent(&checkpoint.supervisor)
            .cloned()
            .ok_or_else(|| invalid_state("the supervisor is not in the organization"))?;
        self.wait_for_reported_stops(rec.workspace_id).await;
        self.wait_for_free_workspace(rec.workspace_id).await;
        let project = rec.project_id;
        let (settings, _) =
            self.storage.call(move |c| settings_of(c, project)).await.map_err(errors::storage)?;
        let available: Vec<String> = profiles::profiles(&self.catalogs().await, &settings.estimates)
            .iter()
            .map(profiles::Profile::describe)
            .collect();
        let prompt = org::render_supervisor(&state, &checkpoint, &entries, &available);
        let provider: AgentProvider = agent
            .provider
            .parse()
            .map_err(|_| invalid_state(format!("unknown provider {}", agent.provider)))?;
        // Continue the supervisor's own conversation when it can be resumed.
        let previous = state.sessions.get(&agent.key).copied();
        let resumable = match previous {
            Some(s) => self
                .storage
                .call(move |c| sessions::get(c, s))
                .await
                .ok()
                .filter(|s| {
                    s.state.is_terminal()
                        && switchyard_agents::capabilities(s.provider).resume_session.is_supported()
                })
                .map(|s| s.id),
            None => None,
        };
        let session = match resumable {
            Some(id) => self.agents.resume(id, None).await?,
            None => {
                self.agents
                    .start(
                        provider,
                        SessionAccess::Review,
                        rec.workspace_id,
                        Some(project),
                        None,
                        agent.model.clone(),
                    )
                    .await?
            }
        };
        let session_id = session.id;
        let key = agent.key.clone();
        let recorded = commit_checked(&self.storage, &self.bus, move |tx| {
            let rec = org_store::get(tx, task_id)?;
            let mut state = state_of(&rec)?;
            if !matches!(&state.activity, Activity::Supervising { checkpoint: c } if c.supervisor == key) {
                return Ok(Err(invalid_state("the organization moved on")));
            }
            state.sessions.insert(key.clone(), session_id);
            state.turn_after_seq = switchyard_storage::events::last_seq(tx)?.unwrap_or(0);
            let value = serde_json::to_value(&state).map_err(corrupt)?;
            org_store::update(
                tx,
                task_id,
                rec.revision,
                &rec.status,
                rec.reason.as_deref(),
                &value,
                false,
                now_ms(),
            )?;
            Ok(Ok(((), Vec::new())))
        })
        .await;
        if let Err(e) = recorded {
            self.stop_session(session_id).await;
            return Err(e);
        }
        if let Err(e) = self.agents.send(session_id, prompt, false).await {
            self.stop_session(session_id).await;
            return Err(e);
        }
        Ok(())
    }

    /// The assistant text of a supervision turn.
    pub(super) async fn supervision_text(
        &self,
        session_id: Uuid,
        after: i64,
        until: i64,
        result: Option<String>,
    ) -> Result<String, ProtocolError> {
        let mut text = String::new();
        let mut after = after;
        loop {
            let page = self
                .bus
                .read(EventFilter {
                    after_seq: Some(after),
                    session_id: Some(session_id),
                    event_types: vec!["agent.message".into()],
                    limit: 500,
                    ..Default::default()
                })
                .await
                .map_err(|e| errors::internal("journal read failed", e))?;
            let full = page.len() == 500;
            for e in page.iter().filter(|e| e.seq < until) {
                if let Ok(DomainEvent::AgentMessage { text: t, .. }) = e.decode() {
                    text.push_str(&t);
                    text.push_str("\n\n");
                }
            }
            match page.last() {
                Some(last) if full && last.seq < until => after = last.seq,
                _ => break,
            }
        }
        if let Some(r) = result {
            text.push_str(&r);
        }
        Ok(text)
    }

    /// Turns a supervisor's text into a decision input: the parsed directive,
    /// Core's provider/model choice for every requested agent, ids for new
    /// agents and tasks, and the review gate before delivery.
    async fn supervised_input(&self, rec: &OrgRecord, state: &OrgState, agent: &str, text: &str) -> Input {
        let now = now_ms();
        let mut directive = org::parse_directive(text).map_err(|e| e.to_string());
        let mut selections = BTreeMap::new();
        let mut new_ids = BTreeMap::new();
        if let Ok(d) = &directive {
            let project = rec.project_id;
            let settings =
                self.storage.call(move |c| settings_of(c, project)).await.map(|s| s.0).unwrap_or_default();
            let profiles = profiles::profiles(&self.catalogs().await, &settings.estimates);
            let coder = state
                .agents
                .iter()
                .chain(std::iter::empty())
                .find(|a| a.kind == AgentKind::Specialist && a.capabilities.iter().any(|c| c == "coding"))
                .and_then(|a| a.provider.parse::<AgentProvider>().ok());
            for a in &d.agents {
                let reviewer = a.capabilities.iter().any(|c| c == "review");
                let req = Requirement {
                    skills: a.capabilities.clone(),
                    supervisor: a.kind == AgentKind::Supervisor,
                    requested_provider: a.provider.clone(),
                    requested_model: a.model.clone(),
                    independent_of: if reviewer { coder } else { None },
                };
                let choice = profiles::select(&profiles, &req, &settings.overrides).map(|c| Selected {
                    provider: c.provider.as_str().into(),
                    model: c.model,
                    rationale: c.rationale,
                });
                selections.insert(a.key.clone(), choice);
                new_ids.insert(format!("agent:{}", a.key), new_id());
            }
            for t in &d.tasks {
                new_ids.insert(format!("task:{}", t.key), new_id());
            }
            if d.done
                && let Activity::Supervising { checkpoint } = &state.activity
                && checkpoint.task == org::ROOT
                && let Err(why) = self.review_gate(rec).await
            {
                directive = Err(why);
            }
        }
        if let Ok(d) = &mut directive {
            // Keep the directive's own shape; nothing else to normalize.
            let _: &mut Directive = d;
        }
        Input::Supervised { agent: agent.into(), directive, selections, new_ids, now_ms: now }
    }

    /// Before the objective goes to delivery: if the project requires a review
    /// (M7 settings), an approved review in the organization must have seen
    /// exactly the current content (ADR 0029).
    async fn review_gate(&self, rec: &OrgRecord) -> Result<(), String> {
        let (project, root, ws) = (rec.project_id, rec.task_id, rec.workspace_id);
        let (required, review) = self
            .storage
            .call(move |c| {
                let required = switchyard_storage::integrations::settings(c, project)?.require_review;
                let review = super::latest_review_in_tree(c, root)?;
                Ok((required, review))
            })
            .await
            .map_err(|e| e.to_string())?;
        if !required {
            return Ok(());
        }
        let ask = "this project requires an independent review of the final content before delivery: add a review \
                   task, owned by a specialist, that depends on all other work";
        let Some((outcome, tree, review_ws)) = review else { return Err(ask.into()) };
        if outcome.as_deref() != Some("review: approved") || review_ws != ws {
            return Err(format!("{ask} (the latest review did not approve)"));
        }
        let w = self.workspaces.record(ws).await.map_err(|e| e.message)?;
        let current = self.workspaces.content_tree(&w).await.map_err(|e| e.to_string())?;
        if tree.as_deref() != Some(current.as_str()) {
            return Err(format!("{ask} (the workspace changed after the latest approved review)"));
        }
        Ok(())
    }

    async fn evidence(&self, rec: &OrgRecord, state: &OrgState, task: &str) -> Evidence {
        let Some(base) = rec.base_commit.clone() else { return Evidence::default() };
        let Ok(w) = self.workspaces.record(rec.workspace_id).await else { return Evidence::default() };
        let changed = match self.workspaces.changed_files(&w, &base).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(?e, "cannot read the workspace's changed files");
                return Evidence::default();
            }
        };
        let Some(t) = state.task(task) else { return Evidence::default() };
        // A file belongs to this task's scope, or to other work that is not cancelled.
        let others: Vec<&OrgTask> = state
            .tasks
            .iter()
            .filter(|o| o.key != task && o.work.changes_files() && o.state != TaskState::Cancelled)
            .collect();
        let outside = if t.work.changes_files() {
            changed
                .iter()
                .filter(|f| !org::in_scope(f, &t.scope) && !others.iter().any(|o| org::in_scope(f, &o.scope)))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Evidence { changed_files: changed, outside_scope: outside }
    }

    // --- the driver ----------------------------------------------------------------

    async fn org_of_session(
        &self,
        session_id: Uuid,
    ) -> Option<(OrgRecord, OrgState, Option<store::AssignmentRecord>)> {
        let found = self
            .storage
            .call(move |c| {
                if let Some(a) = store::active_assignment_for_session(c, session_id)?
                    && let Some(rec) = org_store::for_task(c, a.task_id)?
                {
                    let state = state_of(&rec)?;
                    return Ok(Some((rec, state, Some(a))));
                }
                for rec in org_store::active(c)? {
                    let state = state_of(&rec)?;
                    if state.sessions.values().any(|s| *s == session_id) {
                        return Ok(Some((rec, state, None)));
                    }
                }
                Ok(None)
            })
            .await;
        found.ok().flatten()
    }

    async fn react_org(&self, e: &EventEnvelope, monitor: &Monitor) {
        let Ok(event) = e.decode() else { return };
        let seq = e.seq;
        match &event {
            DomainEvent::AgentTurnCompleted { .. }
            | DomainEvent::AgentExited { .. }
            | DomainEvent::AgentFailed { .. } => {
                let (result, usage, turn_cost_usd) = match &event {
                    DomainEvent::AgentTurnCompleted { result, usage, turn_cost_usd, .. } => {
                        (result.clone(), usage.clone(), *turn_cost_usd)
                    }
                    _ => (None, None, None),
                };
                let Some(session) = e.context.session_id else { return };
                let Some((rec, state, assignment)) = self.org_of_session(session).await else { return };
                if seq <= rec.last_event_seq {
                    return;
                }
                if assignment.is_some() {
                    if let DomainEvent::AgentTurnCompleted { usage, turn_cost_usd, .. } = &event {
                        let tokens =
                            usage.as_ref().and_then(|u| u.get("total_tokens")).and_then(Value::as_u64);
                        if tokens.is_some() || turn_cost_usd.is_some() {
                            let _ = self
                                .apply_org(
                                    rec.task_id,
                                    Input::Usage { cost_usd: *turn_cost_usd, tokens },
                                    None,
                                )
                                .await;
                        }
                    }
                    return;
                }
                let Activity::Supervising { checkpoint } = &state.activity else { return };
                if state.sessions.get(&checkpoint.supervisor) != Some(&session) {
                    return;
                }
                let input = match &event {
                    DomainEvent::AgentTurnCompleted { interrupted: false, .. } => {
                        let text =
                            match self.supervision_text(session, state.turn_after_seq, seq, result).await {
                                Ok(t) => t,
                                Err(err) => {
                                    tracing::error!(?err, "cannot read a supervision turn");
                                    return;
                                }
                            };
                        let tokens =
                            usage.as_ref().and_then(|u| u.get("total_tokens")).and_then(Value::as_u64);
                        if tokens.is_some() || turn_cost_usd.is_some() {
                            let _ = self
                                .apply_org(
                                    rec.task_id,
                                    Input::Usage { cost_usd: turn_cost_usd, tokens },
                                    None,
                                )
                                .await;
                        }
                        self.supervised_input(&rec, &state, &checkpoint.supervisor, &text).await
                    }
                    DomainEvent::AgentTurnCompleted { .. } => {
                        Input::Interrupted { reason: "the supervision turn was interrupted".into() }
                    }
                    DomainEvent::AgentExited { stop_requested: true, .. } => return,
                    DomainEvent::AgentExited { final_state, .. } => Input::Interrupted {
                        reason: format!("the supervisor's session ended ({final_state})"),
                    },
                    DomainEvent::AgentFailed { reason, .. } => {
                        Input::Interrupted { reason: format!("the supervisor's session failed ({reason})") }
                    }
                    _ => return,
                };
                if let Err(err) = self.apply_org_stopping(rec.task_id, input, seq, session).await {
                    tracing::error!(?err, "cannot apply a supervision turn");
                }
            }
            DomainEvent::AgentToolStarted { tool_name, input, .. }
            | DomainEvent::AgentPermissionRequested { tool_name, input, .. } => {
                let Some(session) = e.context.session_id else { return };
                let Some((rec, state, Some(a))) = self.org_of_session(session).await else { return };
                let Some(t) = state.task_by_id(a.task_id) else { return };
                if !matches!(&state.activity, Activity::Working { task, interrupting: false, .. } if *task == t.key)
                    || !t.work.changes_files()
                {
                    return;
                }
                let Ok(w) = self.workspaces.record(rec.workspace_id).await else { return };
                let outside: Vec<String> = changed_paths(tool_name, input, &w.path)
                    .into_iter()
                    .filter(|p| !org::in_scope(p, &t.scope))
                    .collect();
                if !outside.is_empty() {
                    let input = Input::Observed {
                        task: t.key.clone(),
                        observation: Observation::OutsideScope { paths: outside },
                    };
                    if let Err(err) = self.apply_org(rec.task_id, input, None).await {
                        tracing::error!(?err, "cannot record an observation");
                    }
                }
            }
            DomainEvent::AgentToolFailed { rejected: false, .. } => {
                let Some(session) = e.context.session_id else { return };
                let Some((rec, state, Some(a))) = self.org_of_session(session).await else { return };
                let Some(t) = state.task_by_id(a.task_id) else { return };
                let n = monitor.count(a.id);
                if n == MAX_TOOL_FAILURES {
                    let input = Input::Observed {
                        task: t.key.clone(),
                        observation: Observation::RepeatedFailures { count: n },
                    };
                    let _ = self.apply_org(rec.task_id, input, None).await;
                }
            }
            DomainEvent::TaskAssignmentEnded { assignment_id, status, outcome } => {
                let id = *assignment_id;
                let found = self
                    .storage
                    .call(move |c| {
                        let a = store::get_assignment(c, id)?;
                        let Some(rec) = org_store::for_task(c, a.task_id)? else { return Ok(None) };
                        let summary = store::messages_for_task(c, a.task_id)?
                            .into_iter()
                            .rev()
                            .find(|m| m.assignment_id == Some(a.id) && m.sender == "agent")
                            .map(|m| m.payload["report"].clone());
                        Ok(Some((a, rec, summary)))
                    })
                    .await;
                let Ok(Some((a, rec, report))) = found else { return };
                if seq <= rec.last_event_seq || !a.step_key.as_deref().is_some_and(|k| k.starts_with("org:"))
                {
                    return;
                }
                // Core's own hand-off after a question or an unusable turn: that turn
                // was already given to the organization; counting it again would
                // fail the next turn of the same task before it starts.
                if status == "cancelled" && outcome.as_deref() == Some(HANDED_OFF) {
                    return;
                }
                let Ok(state) = state_of(&rec) else { return };
                let Some(t) = state.task_by_id(a.task_id) else { return };
                if !matches!(&state.activity, Activity::Working { task, .. } if *task == t.key) {
                    return;
                }
                let report = report.unwrap_or(Value::Null);
                let summary = report["summary"].as_str().unwrap_or("no summary").to_owned();
                let outcome = outcome.clone().unwrap_or_default();
                let interrupting = matches!(&state.activity, Activity::Working { interrupting: true, .. });
                let worker = match status.as_str() {
                    "reported" if outcome == "blocker" => WorkerOutcome::Blocker { summary },
                    "reported" if outcome == "reported: proposal" => WorkerOutcome::Challenge {
                        summary,
                        details: report["details"].as_str().map(str::to_owned),
                    },
                    "reported" if outcome.starts_with("review: ") => {
                        let result = match outcome.trim_start_matches("review: ") {
                            "approved" => ReviewResult::Approved,
                            "changes_requested" => ReviewResult::ChangesRequested,
                            "rejected" => ReviewResult::Rejected,
                            _ => ReviewResult::Blocked,
                        };
                        WorkerOutcome::Reviewed { result, summary }
                    }
                    "reported" => WorkerOutcome::Delivered { summary },
                    "cancelled" if interrupting => WorkerOutcome::Interrupted { reason: outcome },
                    _ => WorkerOutcome::Failed { reason: outcome },
                };
                let evidence = self.evidence(&rec, &state, &t.key).await;
                let input =
                    Input::Worked { task: t.key.clone(), outcome: worker, evidence, now_ms: now_ms() };
                if let Err(err) = self.apply_org(rec.task_id, input, Some(seq)).await {
                    tracing::error!(?err, "cannot apply a work turn");
                }
            }
            DomainEvent::TaskMessageRecorded { kind, sender, assignment_id: Some(id), summary, .. }
                if (kind == "info_request" && sender == "agent")
                    || (kind == "malformed_report" && sender == "core") =>
            {
                let id = *id;
                let found = self
                    .storage
                    .call(move |c| {
                        let a = store::get_assignment(c, id)?;
                        Ok(org_store::for_task(c, a.task_id)?.map(|r| (a, r)))
                    })
                    .await;
                let Ok(Some((a, rec))) = found else { return };
                if seq <= rec.last_event_seq
                    || a.status != AssignmentStatus::Active
                    || !a.step_key.as_deref().is_some_and(|k| k.starts_with("org:"))
                {
                    return;
                }
                let Ok(state) = state_of(&rec) else { return };
                let Some(t) = state.task_by_id(a.task_id) else { return };
                let text = summary.clone().unwrap_or_else(|| "no details".into());
                let outcome = if kind == "info_request" {
                    WorkerOutcome::Question { summary: text }
                } else {
                    WorkerOutcome::Failed { reason: format!("no usable report: {text}") }
                };
                let evidence = self.evidence(&rec, &state, &t.key).await;
                // The turn is over for the organization: free the workspace (and the
                // task) first, so the next step never starts beside a session that is
                // still stopping. The cancellation's own event comes later and finds
                // the organization moved on.
                let _ = self.cancel_assignment(a.id, Some(HANDED_OFF.into()), ACTOR.into()).await;
                let input = Input::Worked { task: t.key.clone(), outcome, evidence, now_ms: now_ms() };
                if let Err(err) = self.apply_org(rec.task_id, input, Some(seq)).await {
                    tracing::error!(?err, "cannot apply a work turn");
                }
            }
            DomainEvent::TaskStatusChanged { to, .. } if matches!(to.as_str(), "cancelled" | "failed") => {
                let Some(task) = e.context.task_id else { return };
                let found = self.storage.call(move |c| org_store::find(c, task)).await;
                let Ok(Some(rec)) = found else {
                    // One of an organization's own tasks, closed by the user.
                    let owner = self
                        .storage
                        .call(move |c| {
                            for rec in org_store::active(c)? {
                                let state = state_of(&rec)?;
                                if let Some(t) = state.task_by_id(task) {
                                    return Ok(Some((rec, t.key.clone())));
                                }
                            }
                            Ok(None)
                        })
                        .await;
                    let Ok(Some((rec, key))) = owner else { return };
                    if seq <= rec.last_event_seq {
                        return;
                    }
                    let input = Input::TaskClosed { task: key, status: to.clone(), now_ms: now_ms() };
                    if let Err(err) = self.apply_org(rec.task_id, input, Some(seq)).await {
                        tracing::warn!(?err, "organization did not take a closed subtask");
                    }
                    return;
                };
                if seq <= rec.last_event_seq
                    || !matches!(rec.status.as_str(), "running" | "waiting" | "paused")
                {
                    return;
                }
                let _ = self
                    .apply_org(
                        rec.task_id,
                        Input::Cancel { reason: format!("the objective's task was {to}") },
                        Some(seq),
                    )
                    .await;
            }
            _ => {}
        }
    }

    /// Applies a supervision result and stops the supervisor's session before
    /// anything else runs in the workspace.
    async fn apply_org_stopping(
        &self,
        task_id: Uuid,
        input: Input,
        seq: i64,
        session: Uuid,
    ) -> Result<(), ProtocolError> {
        let lock = self.workflow_lock(task_id);
        let guard = lock.lock().await;
        let f = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            if !org_store::advance_seq(tx, task_id, seq)? {
                return Ok(Ok((None, drafts)));
            }
            let f = check!(decide_in_tx(tx, task_id, &input, Some(seq), now_ms(), &mut drafts)?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(guard);
        let mut f = f.unwrap_or(Followup { action: Action::None, assignment: None, stop_session: None });
        f.stop_session = Some(session);
        self.perform_org(task_id, f);
        Ok(())
    }

    /// At startup: a turn that was running when Core stopped did not survive.
    /// The organization waits for the user (agents are never restarted
    /// automatically); this runs before the journal replay, so a replayed end
    /// of that turn finds nothing running and changes nothing.
    async fn recover_orgs(&self) {
        let active = match self.storage.call(|c| org_store::active(c)).await {
            Ok(a) => a,
            Err(err) => {
                tracing::error!(?err, "cannot read organizations to recover");
                return;
            }
        };
        for rec in active {
            let Ok(state) = state_of(&rec) else { continue };
            if state.activity != Activity::Idle {
                tracing::info!(task = %rec.task_id, "an organization's turn was interrupted by a restart");
                let input = Input::Interrupted { reason: "Core restarted while a turn was running".into() };
                if let Err(err) = self.apply_org(rec.task_id, input, None).await {
                    tracing::error!(?err, "cannot record an interrupted organization turn");
                }
            }
        }
    }

    /// Delivery sends the finished objective back to the organization (for
    /// example after a failed validation): the workflow leaves its delivery
    /// stage and the lead supervisor continues with `note`. The open
    /// integration stays and is adopted when the organization finishes again.
    pub(super) async fn reopen_organization(
        &self,
        task_id: Uuid,
        note: Option<String>,
        actor: String,
    ) -> Result<WorkflowInfo, ProtocolError> {
        let text = note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| {
            "The delivery needs another round: check the integration's blockers and validation.".into()
        });
        let lock = self.workflow_lock(task_id);
        let guard = lock.lock().await;
        let f = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let w = wf_store::get_workflow(tx, task_id)?;
            let wstate: WorkflowState = serde_json::from_value(w.state.clone()).map_err(corrupt)?;
            if wstate.stage != Some(Stage::Delivery) || wstate.status.is_final() {
                return Ok(Err(invalid_state("the organization is not delivering; nothing to send back")));
            }
            let reason = format!("sent back by {actor}: {text}");
            let back = WorkflowState {
                status: WorkflowStatus::Running,
                reason: Some(reason.clone()),
                ..Default::default()
            };
            let value = serde_json::to_value(&back).map_err(corrupt)?;
            wf_store::update_workflow(
                tx,
                task_id,
                w.revision,
                &wf_store::WorkflowUpdate {
                    status: "running",
                    stage: None,
                    step_key: None,
                    reason: Some(&reason),
                    state: &value,
                    workspace_id: None,
                    ended: false,
                },
                now,
            )?;
            let root = store::get_task(tx, task_id)?;
            if root.status == TaskStatus::AwaitingReview {
                check!(set_status(
                    tx,
                    task_id,
                    TaskStatus::InProgress,
                    Some("sent back to the organization".into()),
                    None,
                    None,
                    now,
                    &mut drafts
                )?);
            }
            drafts.push(draft(DomainEvent::WorkflowResumed { by: actor.clone() }, &task_context(&root))?);
            let f = check!(decide_in_tx(
                tx,
                task_id,
                &Input::Reopen { text: text.clone(), now_ms: now },
                None,
                now,
                &mut drafts
            )?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(guard);
        if let Some(f) = f {
            self.perform_org(task_id, f);
        }
        self.workflow_required(task_id).await
    }

    // --- operator inputs -------------------------------------------------------------

    pub(super) async fn org_operator(&self, task_id: Uuid, input: Input) -> Result<(), ProtocolError> {
        let rec = self.storage.call(move |c| org_store::get(c, task_id)).await.map_err(errors::storage)?;
        let state = state_of(&rec).map_err(errors::storage)?;
        org::decide(&state, &input).map_err(|e| invalid_state(e.to_string()))?;
        self.apply_org(task_id, input, None).await
    }

    /// Feeds the organization every one of its tasks whose M6 task the user
    /// closed (cancelled or failed) while it still counts it open. `true` if any.
    pub(super) async fn org_catch_up_closed(&self, task_id: Uuid) -> Result<bool, ProtocolError> {
        let closed = self
            .storage
            .call(move |c| {
                let rec = org_store::get(c, task_id)?;
                let state = state_of(&rec)?;
                let mut out = Vec::new();
                for t in &state.tasks {
                    let (Some(id), false, Some(_)) = (t.task_id, t.state.is_final(), t.parent.as_ref())
                    else {
                        continue;
                    };
                    let m6 = store::get_task(c, id)?;
                    if matches!(m6.status, TaskStatus::Cancelled | TaskStatus::Failed) {
                        out.push((t.key.clone(), m6.status.to_string()));
                    }
                }
                Ok(out)
            })
            .await
            .map_err(errors::storage)?;
        let any = !closed.is_empty();
        for (task, status) in closed {
            self.apply_org(task_id, Input::TaskClosed { task, status, now_ms: now_ms() }, None).await?;
        }
        Ok(any)
    }

    pub async fn answer_organization(
        &self,
        task_id: Uuid,
        text: String,
    ) -> Result<Option<OrganizationInfo>, ProtocolError> {
        let text = text.trim().to_owned();
        if text.is_empty() || text.chars().count() > 4000 {
            return Err(invalid_params("the answer must be 1 to 4000 characters"));
        }
        self.org_operator(task_id, Input::Answer { text, now_ms: now_ms() }).await?;
        self.organization(task_id).await
    }

    pub async fn organization(&self, task_id: Uuid) -> Result<Option<OrganizationInfo>, ProtocolError> {
        self.storage
            .call(move |c| {
                let Some(rec) = org_store::find(c, task_id)? else { return Ok(None) };
                let state = state_of(&rec)?;
                let entries = org::current(&entries_of(c, task_id)?);
                let created: BTreeMap<i64, i64> =
                    org_store::context(c, task_id)?.into_iter().map(|r| (r.seq, r.created_at_ms)).collect();
                Ok(Some(info(&rec, &state, &entries, &created)))
            })
            .await
            .map_err(errors::storage)
    }
}

fn info(
    rec: &OrgRecord,
    s: &OrgState,
    entries: &[ContextEntry],
    created: &BTreeMap<i64, i64>,
) -> OrganizationInfo {
    let activity = match &s.activity {
        Activity::Idle => OrgActivity { kind: "idle".into(), agent: None, task: None, items: Vec::new() },
        Activity::Supervising { checkpoint } => OrgActivity {
            kind: "supervising".into(),
            agent: Some(checkpoint.supervisor.clone()),
            task: Some(checkpoint.task.clone()),
            items: checkpoint.items.iter().map(item_label).collect(),
        },
        Activity::Working { task, agent, .. } => OrgActivity {
            kind: "working".into(),
            agent: Some(agent.clone()),
            task: Some(task.clone()),
            items: Vec::new(),
        },
    };
    OrganizationInfo {
        task_id: rec.task_id,
        workspace_id: rec.workspace_id,
        status: s.status.as_str().into(),
        reason: s.reason.clone(),
        agents: s
            .agents
            .iter()
            .map(|a| OrgAgentInfo {
                id: a.id,
                key: a.key.clone(),
                title: a.title.clone(),
                kind: a.kind.as_str().into(),
                reports_to: a.reports_to.clone(),
                responsibility: a.responsibility.clone(),
                scope: a.scope.clone(),
                capabilities: a.capabilities.clone(),
                provider: a.provider.clone(),
                model: a.model.clone(),
                selection: a.selection.clone(),
                status: a.status.as_str().into(),
                created_by: a.created_by.clone(),
                depth: a.depth,
            })
            .collect(),
        tasks: s
            .tasks
            .iter()
            .map(|t| OrgTaskInfo {
                key: t.key.clone(),
                task_id: t.task_id,
                title: t.title.clone(),
                description: t.description.clone(),
                work: t.work.as_str().into(),
                parent: t.parent.clone(),
                owner: t.owner.clone(),
                scope: t.scope.clone(),
                acceptance: t.acceptance.clone(),
                depends_on: t.depends_on.clone(),
                state: t.state.as_str().into(),
                corrections: t.corrections,
                summary: t.summary.clone(),
                created_by: t.created_by.clone(),
            })
            .collect(),
        activity,
        queued_checkpoints: s.queue.len() as u32,
        limits: serde_json::to_value(&s.limits).unwrap_or(Value::Null),
        usage: OrgUsage {
            supervision_turns: s.supervision_turns,
            max_supervision_turns: s.limits.max_supervision_turns,
            agents: s.agents.len() as u32,
            max_agents: s.limits.max_agents,
            tasks: s.tasks.len() as u32,
            max_tasks: s.limits.max_tasks,
            cost_usd: s.cost_usd,
            tokens: s.tokens,
        },
        question: s.question_from.as_ref().and_then(|_| s.reason.clone()),
        question_from: s.question_from.clone(),
        context: entries
            .iter()
            .map(|e| OrgContextEntry {
                seq: e.seq,
                kind: e.kind.as_str().into(),
                status: e.status.as_str().into(),
                text: e.text.clone(),
                task: e.task.clone(),
                source: e.source.clone(),
                supersedes: e.supersedes,
                created_at_ms: created.get(&e.seq).copied().unwrap_or(0),
            })
            .collect(),
        final_summary: s.final_summary.clone(),
        created_at_ms: rec.created_at_ms,
        updated_at_ms: rec.updated_at_ms,
    }
}

/// Decides on `input` for the organization of `task_id` inside `tx` and
/// writes everything the decision implies. `None` when nothing changed.
fn decide_in_tx(
    tx: &Connection,
    task_id: Uuid,
    input: &Input,
    _seq: Option<i64>,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<Option<Followup>> {
    let rec = org_store::get(tx, task_id)?;
    let state = state_of(&rec)?;
    let mut d = match org::decide(&state, input) {
        Ok(d) => d,
        Err(e) => return Ok(Err(invalid_state(e.to_string()))),
    };
    if d.state == state && d.action == Action::None && d.changes.is_empty() && d.context.is_empty() {
        return Ok(Ok(None));
    }
    let root = store::get_task(tx, task_id)?;
    let ctx = task_context(&root);
    let by = |key: &str| format!("{ACTOR}:{key}");
    let author = match input {
        Input::Supervised { agent, .. } => agent.clone(),
        _ => "core".into(),
    };
    let supervisor_session = match input {
        Input::Supervised { agent, .. } => state.sessions.get(agent).copied(),
        _ => None,
    };
    // New agents.
    for c in &d.changes {
        if let Change::AgentAdded { key } = c
            && let Some(a) = d.state.agent(key)
        {
            drafts.push(draft(
                DomainEvent::OrganizationAgentAdded {
                    agent_id: a.id,
                    key: a.key.clone(),
                    title: a.title.clone(),
                    kind: a.kind.as_str().into(),
                    reports_to: a.reports_to.clone(),
                    provider: a.provider.clone(),
                    model: a.model.clone(),
                    selection: a.selection.clone(),
                    added_by: a.created_by.clone(),
                },
                &ctx,
            )?);
        }
    }
    // New subtasks: proposals on their coordination task, decided by Core.
    if !d.created_tasks.is_empty() {
        let summary = match input {
            Input::Supervised { directive: Ok(dir), .. } => dir.summary.clone(),
            _ => String::new(),
        };
        let mut by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in &d.created_tasks {
            let parent = d.state.task(k).and_then(|t| t.parent.clone()).unwrap_or_else(|| org::ROOT.into());
            by_parent.entry(parent).or_default().push(k.clone());
        }
        for (parent_key, keys) in by_parent {
            let Some(parent_id) = d.state.task(&parent_key).and_then(|t| t.task_id) else {
                return Ok(Err(invalid_state(format!("coordination task {parent_key} has no task"))));
            };
            let parent = store::get_task(tx, parent_id)?;
            let m = new_message(
                &parent,
                None,
                MessageKind::DelegationRequest,
                "agent",
                supervisor_session,
                "task",
                json!({ "by": by(&author), "summary": summary, "tasks": keys }),
                None,
                "recorded",
                now,
            );
            record_message(
                tx,
                &parent,
                &m,
                Some(&format!("{} delegated {} task(s)", author, keys.len())),
                drafts,
            )?;
            for (position, key) in keys.iter().enumerate() {
                let Some(t) = d.state.task(key).cloned() else { continue };
                let Some(id) = t.task_id else { continue };
                let mut description = t.description.clone().unwrap_or_default();
                if !t.scope.is_empty() && t.work.changes_files() {
                    description.push_str(&format!("\n\nScope: {}", t.scope.join(", ")));
                }
                let proposal = ProposalRecord {
                    id: new_id(),
                    project_id: root.project_id,
                    task_id: parent_id,
                    message_id: m.id,
                    position: position as u32,
                    proposal: json!({ "title": t.title, "description": description, "kind": t.work.task_kind(),
                        "acceptance_criteria": t.acceptance, "depends_on": [] , "organization_key": t.key, "owner": t.owner }),
                    status: ProposalStatus::Pending,
                    decided_by: None,
                    decision_note: None,
                    created_task_id: None,
                    created_at_ms: now,
                    decided_at_ms: None,
                };
                store::insert_proposal(tx, &proposal)?;
                let record = TaskRecord {
                    id,
                    project_id: root.project_id,
                    parent_task_id: Some(parent_id),
                    title: t.title.clone(),
                    description: (!description.trim().is_empty()).then(|| description.trim().to_owned()),
                    kind: t.work.task_kind(),
                    priority: root.priority,
                    acceptance_criteria: t.acceptance.clone(),
                    status: TaskStatus::Draft,
                    status_reason: None,
                    result: None,
                    failure: None,
                    origin: "proposal".into(),
                    origin_proposal_id: Some(proposal.id),
                    revision: 1,
                    created_at_ms: now,
                    updated_at_ms: now,
                    completed_at_ms: None,
                };
                check!(insert_task_in_tx(tx, &record, drafts)?);
                for dep in &t.depends_on {
                    if let Some(dep_id) = d.state.task(dep).and_then(|x| x.task_id) {
                        check!(add_dependency_in_tx(tx, &record, dep_id, now, drafts)?);
                    }
                }
                store::decide_proposal(
                    tx,
                    proposal.id,
                    ProposalStatus::Approved,
                    &by(&author),
                    Some("decided by Switchyard within the organization's limits"),
                    Some(id),
                    now,
                )?;
                drafts.push(draft(
                    DomainEvent::TaskProposalDecided {
                        proposal_id: proposal.id,
                        decision: "approved".into(),
                        created_task_id: Some(id),
                    },
                    &task_context(&parent),
                )?);
                check!(set_status(
                    tx,
                    id,
                    TaskStatus::Ready,
                    Some("delegated in the organization".into()),
                    None,
                    None,
                    now,
                    drafts
                )?);
                check!(refresh_readiness(tx, &record, now, drafts)?);
                drafts.push(draft(
                    DomainEvent::OrganizationTaskDelegated {
                        key: t.key.clone(),
                        subtask_id: id,
                        title: t.title.clone(),
                        work: t.work.as_str().into(),
                        owner: t.owner.clone(),
                        parent: t.parent.clone(),
                        delegated_by: author.clone(),
                    },
                    &ctx,
                )?);
            }
        }
    }
    // Accepted / cancelled tasks.
    for (key, accepted) in &d.closed_tasks {
        let Some(t) = d.state.task(key).cloned() else { continue };
        let Some(id) = t.task_id else { continue };
        if id == task_id {
            continue; // The objective is completed by delivery.
        }
        let current = store::get_task(tx, id)?;
        if current.status.is_terminal() {
            continue;
        }
        if *accepted {
            if current.status == TaskStatus::InProgress {
                check!(set_status(
                    tx,
                    id,
                    TaskStatus::AwaitingReview,
                    Some("reported to its supervisor".into()),
                    None,
                    None,
                    now,
                    drafts
                )?);
            }
            let result = json!({ "summary": t.summary, "accepted_by": by(&author) });
            check!(set_status(
                tx,
                id,
                TaskStatus::Completed,
                Some(format!("accepted by {}", by(&author))),
                Some(&result),
                None,
                now,
                drafts
            )?);
        } else {
            check!(set_status(
                tx,
                id,
                TaskStatus::Cancelled,
                Some(format!("cancelled by {}", by(&author))),
                None,
                None,
                now,
                drafts
            )?);
        }
    }
    // Journal the other changes.
    for c in &d.changes {
        let event = match c {
            Change::Assessed { task, verdict, by, text } => DomainEvent::OrganizationAssessed {
                key: task.clone(),
                verdict: verdict.as_str().into(),
                by: by.clone(),
                instructions: text.clone(),
            },
            Change::Observed { task, text } => DomainEvent::OrganizationObserved {
                key: task.clone(),
                text: text.clone(),
                interrupted: matches!(d.action, Action::Interrupt { .. }),
            },
            Change::Escalated { by, to, question } => DomainEvent::OrganizationEscalated {
                by: by.clone(),
                to: match to {
                    org::EscalateTo::User => "user".into(),
                    org::EscalateTo::Supervisor => "supervisor".into(),
                },
                question: question.clone(),
            },
            Change::DirectiveRejected { by, error } => {
                DomainEvent::OrganizationDirectiveRejected { by: by.clone(), error: error.clone() }
            }
            Change::Waiting { reason } => DomainEvent::OrganizationStatusChanged {
                status: "waiting".into(),
                reason: Some(reason.clone()),
            },
            Change::Paused => DomainEvent::OrganizationStatusChanged {
                status: "paused".into(),
                reason: d.state.reason.clone(),
            },
            Change::Resumed => {
                DomainEvent::OrganizationStatusChanged { status: "running".into(), reason: None }
            }
            Change::Done { summary } => DomainEvent::OrganizationStatusChanged {
                status: "done".into(),
                reason: Some(summary.clone()),
            },
            Change::Cancelled { reason } => DomainEvent::OrganizationStatusChanged {
                status: "cancelled".into(),
                reason: Some(reason.clone()),
            },
            Change::Created | Change::AgentAdded { .. } | Change::TaskDelegated { .. } => continue,
        };
        drafts.push(draft(event, &ctx)?);
    }
    // Shared context.
    if !d.context.is_empty() {
        let mut first = None;
        let mut last = 0;
        for n in &d.context {
            let seq = org_store::append_context(
                tx,
                task_id,
                n.kind.as_str(),
                n.status.as_str(),
                &n.text,
                n.task.as_deref(),
                &n.source,
                n.supersedes,
                now,
            )?;
            first.get_or_insert(seq);
            last = seq;
        }
        drafts.push(draft(
            DomainEvent::OrganizationContextAdded { from_seq: first.unwrap_or(last), to_seq: last },
            &ctx,
        )?);
    }
    // The next action.
    let mut assignment = None;
    match d.action.clone() {
        Action::Supervise { checkpoint } => {
            if let Some(id) = d.state.task(&checkpoint.task).and_then(|t| t.task_id) {
                let t = store::get_task(tx, id)?;
                if t.status == TaskStatus::Ready {
                    check!(set_status(
                        tx,
                        id,
                        TaskStatus::InProgress,
                        Some("its supervisor coordinates it".into()),
                        None,
                        None,
                        now,
                        drafts
                    )?);
                }
            }
            drafts.push(draft(
                DomainEvent::OrganizationSupervisionStarted {
                    supervisor: checkpoint.supervisor.clone(),
                    key: checkpoint.task.clone(),
                    items: checkpoint.items.iter().map(item_label).collect(),
                },
                &ctx,
            )?);
        }
        Action::Work { task, agent, instructions } => {
            let t = d.state.task(&task).cloned();
            let a = d.state.agent(&agent).cloned();
            let (Some(t), Some(a)) = (t, a) else {
                return Ok(Err(invalid_state(format!("work {task} has no owner"))));
            };
            let role = t.work.role().unwrap_or(Role::Implement);
            let id = t.task_id.unwrap_or(task_id);
            let n = store::assignments_for_task(tx, id)?.len() + 1;
            let new = NewAssignment {
                task_id: id,
                role,
                provider: a.provider.clone(),
                model: a.model.clone(),
                workspace_id: rec.workspace_id,
                workspace_project: rec.project_id,
                origin: "workflow",
                step_key: Some(format!("org:{}#{n}", t.key)),
                by: by(&author),
                workspace_review: role == Role::Review,
            };
            tx.execute_batch("SAVEPOINT org_work")?;
            let mark = drafts.len();
            match create_assignment(tx, &new, now, drafts)? {
                Ok(aid) => {
                    tx.execute_batch("RELEASE org_work")?;
                    assignment = Some(aid);
                    drafts.push(draft(
                        DomainEvent::OrganizationWorkStarted {
                            key: t.key.clone(),
                            agent: a.key.clone(),
                            instructions,
                        },
                        &ctx,
                    )?);
                }
                Err(e) => {
                    tx.execute_batch("ROLLBACK TO org_work; RELEASE org_work")?;
                    drafts.truncate(mark);
                    let reason =
                        format!("{} could not be started: {}. Resume to try again.", t.title, e.message);
                    if let Some(tt) = d.state.tasks.iter_mut().find(|x| x.key == t.key) {
                        tt.state = TaskState::Ready;
                        // The turn never started: its instructions go back, so the
                        // retry gets them (they were moved out for this turn).
                        let mut back = std::mem::take(&mut tt.current_instructions);
                        back.append(&mut tt.instructions);
                        tt.instructions = back;
                    }
                    d.state.activity = Activity::Idle;
                    d.state.status = OrgStatus::Waiting;
                    d.state.reason = Some(reason.clone());
                    d.action = Action::None;
                    drafts.push(draft(
                        DomainEvent::OrganizationStatusChanged {
                            status: "waiting".into(),
                            reason: Some(reason),
                        },
                        &ctx,
                    )?);
                }
            }
        }
        Action::Deliver { summary } => {
            if root.status == TaskStatus::InProgress {
                check!(set_status(
                    tx,
                    task_id,
                    TaskStatus::AwaitingReview,
                    Some("the organization finished; ready for delivery".into()),
                    None,
                    None,
                    now,
                    drafts
                )?);
            }
            let w = wf_store::get_workflow(tx, task_id)?;
            let reason = format!("The organization finished: {summary} Preparing its integration.");
            let wstate = WorkflowState {
                status: WorkflowStatus::Running,
                stage: Some(Stage::Delivery),
                delivery: Some("preparing".into()),
                reason: Some(reason.clone()),
                ..Default::default()
            };
            let value = serde_json::to_value(&wstate).map_err(corrupt)?;
            wf_store::update_workflow(
                tx,
                task_id,
                w.revision,
                &wf_store::WorkflowUpdate {
                    status: "running",
                    stage: Some(Stage::Delivery.as_str()),
                    step_key: None,
                    reason: Some(&reason),
                    state: &value,
                    workspace_id: None,
                    ended: false,
                },
                now,
            )?;
            drafts.push(draft(
                DomainEvent::WorkflowDeliveryChanged {
                    phase: "preparing".into(),
                    reason,
                    integration_id: w.integration_id,
                },
                &ctx,
            )?);
        }
        Action::Interrupt { .. } | Action::Stop | Action::None => {}
    }
    // Mirror the organization's status on its workflow (until delivery takes over).
    let wf_status = match d.state.status {
        OrgStatus::Running => Some("running"),
        OrgStatus::Waiting => Some("waiting"),
        OrgStatus::Paused => Some("paused"),
        OrgStatus::Cancelled => Some("cancelled"),
        OrgStatus::Done => None,
    };
    if let Some(s) = wf_status
        && d.state.status != state.status
    {
        let w = wf_store::get_workflow(tx, task_id)?;
        let mut wstate: WorkflowState = serde_json::from_value(w.state.clone()).map_err(corrupt)?;
        wstate.status = WorkflowStatus::parse(s).unwrap_or(WorkflowStatus::Running);
        wstate.reason = d.state.reason.clone();
        let value = serde_json::to_value(&wstate).map_err(corrupt)?;
        wf_store::update_workflow(
            tx,
            task_id,
            w.revision,
            &wf_store::WorkflowUpdate {
                status: s,
                stage: None,
                step_key: None,
                reason: d.state.reason.as_deref(),
                state: &value,
                workspace_id: None,
                ended: s == "cancelled",
            },
            now,
        )?;
        let event = match s {
            "waiting" => DomainEvent::WorkflowWaiting {
                step_key: None,
                reason: d.state.reason.clone().unwrap_or_default(),
            },
            "paused" => DomainEvent::WorkflowPaused { by: "desktop".into() },
            "running" => DomainEvent::WorkflowResumed { by: "desktop".into() },
            _ => DomainEvent::WorkflowCancelled { reason: d.state.reason.clone().unwrap_or_default() },
        };
        drafts.push(draft(event, &ctx)?);
    }
    let value = serde_json::to_value(&d.state).map_err(corrupt)?;
    org_store::update(
        tx,
        task_id,
        rec.revision,
        d.state.status.as_str(),
        d.state.reason.as_deref(),
        &value,
        matches!(d.state.status, OrgStatus::Done | OrgStatus::Cancelled),
        now,
    )?;
    Ok(Ok(Some(Followup { action: d.action, assignment, stop_session: None })))
}

/// Follows the journal for every organization; started after the workflow driver.
pub(crate) fn spawn_org_driver(
    svc: TaskService,
    mut shutdown: watch::Receiver<Option<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = svc
            .storage
            .call(|c| {
                let lowest = org_store::active(c)?.iter().map(|o| o.last_event_seq).min();
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
                    "organization driver cannot read its starting point; organizations will not advance"
                );
                return;
            }
        };
        svc.recover_orgs().await;
        let monitor = Monitor::default();
        let filter = EventFilter { event_types: WATCHED.map(str::to_owned).to_vec(), ..Default::default() };
        let mut sub = svc.bus.subscribe(Some(start), filter);
        loop {
            tokio::select! {
                () = wait_shutdown(&mut shutdown) => break,
                next = sub.next() => match next {
                    Ok(event) => svc.react_org(&event, &monitor).await,
                    Err(RecvError::Closed) => break,
                    Err(err) => {
                        tracing::error!(?err, "organization driver cannot read the journal; retrying");
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
    fn tool_paths_are_relative_to_the_workspace() {
        let ws = "/data/ws/1";
        assert_eq!(
            changed_paths("Write", &json!({"file_path": "/data/ws/1/src/a.rs"}), ws),
            vec!["src/a.rs"]
        );
        assert_eq!(changed_paths("Edit", &json!({"file_path": "/etc/passwd"}), ws), vec!["/etc/passwd"]);
        assert_eq!(
            changed_paths(
                "apply_patch",
                &json!({"changes": [{"path": "/data/ws/1/README.md"}, {"path": "/data/ws/1/src/x"}]}),
                ws
            ),
            vec!["README.md", "src/x"]
        );
        assert!(changed_paths("Read", &json!({"file_path": "/data/ws/1/a"}), ws).is_empty());
        assert!(!org::in_scope("/etc/passwd", &["src".to_owned()]));
    }
}
