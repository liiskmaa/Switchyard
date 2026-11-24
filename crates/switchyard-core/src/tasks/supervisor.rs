//! The task supervisor (ADR 0041): the operator asks about a task or asks
//! for work; a read-only agent answers and may name a workflow role to route
//! the work to. Core checks that request against the workflow and acts only
//! through the workflow's own decision function (or, for a delivered task, by
//! opening a follow-up task). Nothing here pushes, deploys, merges or approves.
//!
//! The supervisor runs in its own read-only workspace (branched from the
//! task's branch): only one live session may use a workspace, and the task's
//! agent may be working in its own. The task's uncommitted changes reach the
//! supervisor as a diff in its prompt.

use std::time::Duration;

use switchyard_agents::{AgentProvider, SessionAccess};
use switchyard_events::{DomainEvent, EventEnvelope, EventFilter, new_id, now_ms};
use switchyard_protocol::methods::{SupervisorInfo, TasksCreateParams, WorkflowsStartParams};
use switchyard_protocol::model::{RoleBindingsPatch, SupervisorMessage, TaskKind};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::StorageError;
use switchyard_storage::supervisor::{self as sup_store, SupervisorMessageRecord, SupervisorRecord};
use switchyard_storage::tasks as store;
use switchyard_storage::workflows as wf_store;
use switchyard_tasks::supervisor::{self as sup, Route, View};
use switchyard_tasks::workflow::{self as wf, Input, Target, WorkflowError, WorkflowStatus};
use switchyard_tasks::{AssignmentStatus, TaskStatus};
use uuid::Uuid;

use super::{TaskService, bounded, check, invalid_params, invalid_state, lookup, task_context};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

/// How long one answer may take before Core gives up on it.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// The most of the task's diff the supervisor sees.
const MAX_DIFF_BYTES: usize = 40_000;
/// Earlier exchanges included in the prompt.
const HISTORY: usize = 6;

fn wire(m: SupervisorMessageRecord) -> SupervisorMessage {
    SupervisorMessage {
        id: m.id,
        sender: m.sender,
        text: m.text,
        route: m.route,
        instruction: m.instruction,
        outcome: m.outcome,
        follow_up_task_id: m.follow_up_task_id,
        by: m.by,
        created_at_ms: m.created_at_ms,
    }
}

/// What Core did with a reply's route.
struct Routed {
    outcome: Option<String>,
    follow_up: Option<Uuid>,
}

impl TaskService {
    pub async fn supervisor(&self, task_id: Uuid) -> Result<SupervisorInfo, ProtocolError> {
        self.storage
            .call(move |c| {
                store::get_task(c, task_id)?;
                let rec = sup_store::get(c, task_id)?;
                Ok(SupervisorInfo {
                    task_id,
                    thinking: sup_store::thinking(c, task_id)?,
                    messages: sup_store::messages(c, task_id)?.into_iter().map(wire).collect(),
                    workspace_id: rec.and_then(|r| r.workspace_id),
                })
            })
            .await
            .map_err(|e| match e {
                StorageError::NotFound(w) => {
                    ProtocolError::new(ErrorCode::NotFound, format!("{w} not found"))
                }
                other => errors::storage(other),
            })
    }

    /// Records the operator's message and starts the supervisor's answer in
    /// the background; the answer arrives as `supervisor.answered`.
    pub async fn ask_supervisor(
        &self,
        task_id: Uuid,
        text: String,
        actor: String,
    ) -> Result<SupervisorInfo, ProtocolError> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(invalid_params("the message is empty"));
        }
        if text.chars().count() > sup::MAX_MESSAGE {
            return Err(invalid_params(format!(
                "the message is longer than {} characters",
                sup::MAX_MESSAGE
            )));
        }
        let by = actor.clone();
        let message = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let t = check!(lookup(store::get_task(tx, task_id), ErrorCode::NotFound)?);
            if sup_store::thinking(tx, task_id)? {
                return Ok(Err(ProtocolError::new(
                    ErrorCode::Conflict,
                    "the supervisor is still answering your last message",
                )));
            }
            let m = SupervisorMessageRecord {
                id: new_id(),
                task_id,
                sender: "operator".into(),
                text: text.clone(),
                route: None,
                instruction: None,
                outcome: None,
                follow_up_task_id: None,
                session_id: None,
                by: Some(by.clone()),
                created_at_ms: now,
            };
            sup_store::insert_message(tx, &m)?;
            let drafts = vec![draft(
                DomainEvent::SupervisorAsked { message_id: m.id, by: by.clone() },
                &task_context(&t),
            )?];
            Ok(Ok((m, drafts)))
        })
        .await?;
        let svc = self.clone();
        tokio::spawn(async move {
            let result = svc.answer(task_id, &message, &actor).await;
            if let Err(e) = result {
                tracing::warn!(%task_id, ?e, "the supervisor could not answer");
                let _ = svc
                    .record_reply(
                        task_id,
                        "core",
                        e.message,
                        None,
                        None,
                        None,
                        Routed { outcome: None, follow_up: None },
                    )
                    .await;
            }
        });
        self.supervisor(task_id).await
    }

    /// One answer: prompt, session, the turn, the route.
    async fn answer(
        &self,
        task_id: Uuid,
        message: &SupervisorMessageRecord,
        actor: &str,
    ) -> Result<(), ProtocolError> {
        let (prompt, provider, model) = self.supervisor_prompt(task_id, &message.text).await?;
        let (session_id, after) = self.supervisor_session(task_id, provider, model).await?;
        let filter = EventFilter {
            event_types: vec!["agent.turn_completed".into(), "agent.exited".into(), "agent.failed".into()],
            ..EventFilter::default()
        };
        let mut sub = self.bus.subscribe(Some(after), filter);
        if let Err(e) = self.agents.send(session_id, prompt, false).await {
            self.stop_session(session_id).await;
            return Err(e);
        }
        let ended = tokio::time::timeout(ANSWER_TIMEOUT, async {
            loop {
                match sub.next().await {
                    Ok(e) if e.context.session_id == Some(session_id) => return Some(e),
                    Ok(_) => {}
                    Err(switchyard_events::RecvError::Closed) => return None,
                    Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            }
        })
        .await;
        let text = match ended {
            Ok(Some(e)) => self.supervisor_turn_text(session_id, after, &e).await,
            Ok(None) => Err("Core is stopping".to_owned()),
            Err(_) => {
                Err(format!("the supervisor did not answer within {} minutes", ANSWER_TIMEOUT.as_secs() / 60))
            }
        };
        self.stop_session(session_id).await;
        let text = match text {
            Ok(t) => t,
            Err(why) => {
                return self
                    .record_reply(
                        task_id,
                        "core",
                        format!("No answer: {why}. Ask again."),
                        Some(session_id),
                        None,
                        None,
                        Routed { outcome: None, follow_up: None },
                    )
                    .await;
            }
        };
        let reply = sup::parse_reply(&text);
        let routed = match (reply.route.target(), reply.instruction.as_deref()) {
            (Some(target), Some(instruction)) => {
                self.route(task_id, target, instruction, reply.title.as_deref(), actor).await
            }
            _ => Routed {
                outcome: reply.invalid.as_ref().map(|w| {
                    format!("Nothing was routed: the supervisor's routing block was not valid ({w}).")
                }),
                follow_up: None,
            },
        };
        let answer =
            if reply.answer.is_empty() { "(no text)".to_owned() } else { bounded(&reply.answer, 20_000) };
        let route = (reply.route != Route::None).then(|| reply.route.as_str().to_owned());
        self.record_reply(task_id, "supervisor", answer, Some(session_id), route, reply.instruction, routed)
            .await
    }

    pub(super) async fn supervisor_turn_text(
        &self,
        session_id: Uuid,
        after: i64,
        e: &EventEnvelope,
    ) -> Result<String, String> {
        match e.decode() {
            Ok(DomainEvent::AgentTurnCompleted { interrupted: false, result, success, reason, .. }) => {
                // The turn's result repeats its final message: use it only when no message was streamed.
                let text =
                    self.supervision_text(session_id, after, e.seq, None).await.map_err(|e| e.message)?;
                let text = if text.trim().is_empty() { result.unwrap_or_default() } else { text };
                // A failed turn with nothing said (a usage limit, say): its reason, not "no block".
                if !success && text.trim().is_empty() {
                    return Err(format!(
                        "the turn failed: {}",
                        reason
                            .as_deref()
                            .map(|r| r.trim_start_matches("failed: "))
                            .unwrap_or("no reason given")
                    ));
                }
                Ok(text)
            }
            Ok(DomainEvent::AgentTurnCompleted { .. }) => Err("the turn was interrupted".into()),
            Ok(DomainEvent::AgentExited { final_state, .. }) => {
                Err(format!("the session ended ({final_state})"))
            }
            Ok(DomainEvent::AgentFailed { reason, .. }) => Err(format!("the session failed ({reason})")),
            _ => Err("unexpected event".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_reply(
        &self,
        task_id: Uuid,
        sender: &'static str,
        text: String,
        session_id: Option<Uuid>,
        route: Option<String>,
        instruction: Option<String>,
        routed: Routed,
    ) -> Result<(), ProtocolError> {
        commit_checked(&self.storage, &self.bus, move |tx| {
            let t = check!(lookup(store::get_task(tx, task_id), ErrorCode::NotFound)?);
            let m = SupervisorMessageRecord {
                id: new_id(),
                task_id,
                sender: sender.into(),
                text: text.clone(),
                route: route.clone(),
                instruction: instruction.clone(),
                outcome: routed.outcome.clone(),
                follow_up_task_id: routed.follow_up,
                session_id,
                by: None,
                created_at_ms: now_ms(),
            };
            sup_store::insert_message(tx, &m)?;
            let drafts = vec![draft(
                DomainEvent::SupervisorAnswered {
                    message_id: m.id,
                    route: route.clone(),
                    outcome: routed.outcome.clone(),
                    follow_up_task_id: routed.follow_up,
                },
                &task_context(&t),
            )?];
            Ok(Ok(((), drafts)))
        })
        .await
    }

    /// The supervisor's session: its last one resumed when the provider
    /// supports it, otherwise a new read-only session in its own workspace.
    /// Returns the session and the journal position before it started.
    /// The task's side workspace (read-only sessions of the supervisor and
    /// the goal check), created from the task's branch on first use and
    /// recorded without touching the supervisor's session.
    pub(super) async fn side_workspace(&self, task_id: Uuid) -> Result<(Uuid, Uuid), ProtocolError> {
        let (task, rec, task_ws) = self
            .storage
            .call(move |c| {
                let t = store::get_task(c, task_id)?;
                let rec = sup_store::get(c, task_id)?;
                let ws = wf_store::find_workflow(c, task_id)?.and_then(|w| w.workspace_id);
                Ok((t, rec, ws))
            })
            .await
            .map_err(errors::storage)?;
        if let Some(id) = rec.as_ref().and_then(|r| r.workspace_id)
            && self.workspaces.record(id).await.is_ok()
        {
            return Ok((id, task.project_id));
        }
        let base = match task_ws {
            Some(id) => self.workspaces.record(id).await.ok().and_then(|w| w.branch),
            None => None,
        };
        let name = format!("supervisor-{}", &task.id.simple().to_string()[..12]);
        let id = self.workspaces.create(task.project_id, Some(name), base).await?.id;
        let record = SupervisorRecord {
            task_id,
            project_id: task.project_id,
            workspace_id: Some(id),
            session_id: None,
            updated_at_ms: now_ms(),
        };
        self.storage.call(move |c| sup_store::upsert(c, &record)).await.map_err(errors::storage)?;
        Ok((id, task.project_id))
    }

    pub(super) async fn supervisor_session(
        &self,
        task_id: Uuid,
        provider: AgentProvider,
        model: Option<String>,
    ) -> Result<(Uuid, i64), ProtocolError> {
        let (task, rec, task_ws) = self
            .storage
            .call(move |c| {
                let t = store::get_task(c, task_id)?;
                let rec = sup_store::get(c, task_id)?;
                let ws = wf_store::find_workflow(c, task_id)?.and_then(|w| w.workspace_id);
                Ok((t, rec, ws))
            })
            .await
            .map_err(errors::storage)?;
        let workspace = match rec.as_ref().and_then(|r| r.workspace_id) {
            Some(id) if self.workspaces.record(id).await.is_ok() => id,
            _ => {
                let base = match task_ws {
                    Some(id) => self.workspaces.record(id).await.ok().and_then(|w| w.branch),
                    None => None,
                };
                let name = format!("supervisor-{}", &task.id.simple().to_string()[..12]);
                self.workspaces.create(task.project_id, Some(name), base).await?.id
            }
        };
        let after = self
            .storage
            .call(|c| Ok(switchyard_storage::events::last_seq(c)?.unwrap_or(0)))
            .await
            .map_err(errors::storage)?;
        let previous = rec.as_ref().filter(|r| r.workspace_id == Some(workspace)).and_then(|r| r.session_id);
        let resumable = match previous {
            Some(s) => self
                .storage
                .call(move |c| switchyard_storage::sessions::get(c, s))
                .await
                .ok()
                .filter(|s| {
                    s.state.is_terminal()
                        && s.provider == provider
                        && switchyard_agents::capabilities(s.provider).resume_session.is_supported()
                })
                .map(|s| s.id),
            None => None,
        };
        let session = match resumable {
            Some(id) => match self.agents.resume(id, None).await {
                Ok(s) => s,
                Err(_) => {
                    self.agents
                        .start(provider, SessionAccess::Review, workspace, Some(task.project_id), None, model)
                        .await?
                }
            },
            None => {
                self.agents
                    .start(provider, SessionAccess::Review, workspace, Some(task.project_id), None, model)
                    .await?
            }
        };
        let record = SupervisorRecord {
            task_id,
            project_id: task.project_id,
            workspace_id: Some(workspace),
            session_id: Some(session.id),
            updated_at_ms: now_ms(),
        };
        if let Err(e) = self.storage.call(move |c| sup_store::upsert(c, &record)).await {
            self.stop_session(session.id).await;
            return Err(errors::storage(e));
        }
        Ok((session.id, after))
    }

    /// The prompt (from Core's records and Git) and the supervisor's provider
    /// and model: the reasoning role's binding.
    async fn supervisor_prompt(
        &self,
        task_id: Uuid,
        message: &str,
    ) -> Result<(String, AgentProvider, Option<String>), ProtocolError> {
        let (t, w, assignments, messages, findings, runs, history) = self
            .storage
            .call(move |c| {
                let t = store::get_task(c, task_id)?;
                let w = wf_store::find_workflow(c, task_id)?;
                let a = store::assignments_for_task(c, task_id)?;
                let m = store::messages_for_task(c, task_id)?;
                let f = store::findings_for_task(c, task_id)?;
                let r = wf_store::test_runs_for_task(c, task_id)?;
                let h = sup_store::messages(c, task_id)?;
                Ok((t, w, a, m, f, r, h))
            })
            .await
            .map_err(errors::storage)?;
        let bindings = match &w {
            Some(rec) if rec.definition == wf::DEFINITION => {
                super::workflow::bindings_of(rec).map_err(errors::storage)?
            }
            _ => self.workflow_settings(t.project_id).await?.bindings,
        };
        let provider: AgentProvider = bindings
            .investigate
            .provider
            .parse()
            .map_err(|_| invalid_state(format!("unknown provider {}", bindings.investigate.provider)))?;
        let mut v = View {
            task_id: t.id.to_string(),
            task_title: t.title.clone(),
            task_description: t.description.clone(),
            task_status: t.status.to_string(),
            acceptance_criteria: t.acceptance_criteria.clone(),
            delivered: t.status == TaskStatus::Completed,
            message: message.to_owned(),
            ..View::default()
        };
        if let Some(rec) = &w {
            let state = super::workflow::state_of(rec).map_err(errors::storage)?;
            v.delivered |= state.status == WorkflowStatus::Delivered;
            v.workflow = Some(format!(
                "{} workflow: {}{}{}{}",
                rec.definition,
                rec.status,
                rec.stage.as_deref().map(|s| format!(", stage {s}")).unwrap_or_default(),
                rec.step_key.as_deref().map(|s| format!(", step {s}")).unwrap_or_default(),
                rec.reason.as_deref().map(|r| format!(". {r}")).unwrap_or_default(),
            ));
            if let Some(wf::Resume::Answer { question }) = &state.on_resume {
                v.open_question = Some(question.clone());
            }
            if rec.definition == wf::DEFINITION {
                if wf::agent_busy(&state) {
                    v.working = state.current.as_ref().map(|s| target_name(s.binding).to_owned());
                }
                for route in [Route::Reasoning, Route::Coding, Route::Review, Route::Tests] {
                    let Some(target) = route.target() else { continue };
                    if v.working.as_deref() == Some(route.as_str()) {
                        continue;
                    }
                    if let Err(e) = wf::decide(&state, &Input::Directed { target }) {
                        v.unavailable.push((route, refusal(&e)));
                    }
                }
            } else {
                for route in [Route::Reasoning, Route::Coding, Route::Review, Route::Tests] {
                    v.unavailable
                        .push((route, "the task runs as an agent organization; ask its lead instead".into()));
                }
            }
            if let Some(ws) = rec.workspace_id
                && let Ok(record) = self.workspaces.record(ws).await
            {
                match self.workspaces.changes_text(&record, MAX_DIFF_BYTES).await {
                    Ok(Some(text)) => v.changes = Some(text),
                    Ok(None) => v.changes = Some("No changes yet.".into()),
                    Err(e) => v.changes = Some(format!("Could not read the changes: {e}")),
                }
            }
        }
        v.steps = assignments
            .iter()
            .map(|a| {
                format!(
                    "{} ({}, {}{}): {}{}",
                    a.step_key.as_deref().unwrap_or("operator assignment"),
                    a.role,
                    a.provider,
                    a.model.as_deref().map(|m| format!(" {m}")).unwrap_or_default(),
                    a.status,
                    a.outcome.as_deref().map(|o| format!(", {o}")).unwrap_or_default()
                )
            })
            .collect();
        v.messages = messages
            .iter()
            .rev()
            .take(10)
            .rev()
            .map(|m| {
                let p = &m.payload;
                let text = p["report"]["summary"]
                    .as_str()
                    .or_else(|| p["report"]["question"].as_str())
                    .or_else(|| p["text"].as_str())
                    .or_else(|| p["error"].as_str())
                    .unwrap_or("");
                format!("{} from {}: {}", m.kind, m.sender, bounded(text, 600))
            })
            .collect();
        v.findings = findings
            .iter()
            .filter(|f| f.status == "open")
            .map(|f| {
                format!(
                    "[{}] {}{}{}",
                    f.severity,
                    f.title,
                    f.path.as_deref().map(|p| format!(" ({p})")).unwrap_or_default(),
                    f.explanation.as_deref().map(|e| format!(": {}", bounded(e, 400))).unwrap_or_default()
                )
            })
            .collect();
        v.tests = runs.last().map(|r| format!("{} for {}: {}", r.status, r.step_key, r.summary));
        // Earlier exchanges (the operator's message now is the last one).
        let mut pairs = Vec::new();
        let mut q: Option<&str> = None;
        for m in history.iter().take(history.len().saturating_sub(1)) {
            match m.sender.as_str() {
                "operator" => q = Some(&m.text),
                _ => {
                    if let Some(question) = q.take() {
                        pairs.push((bounded(question, 1000), bounded(&m.text, 1500)));
                    }
                }
            }
        }
        let skip = pairs.len().saturating_sub(HISTORY);
        v.history = pairs.into_iter().skip(skip).collect();
        Ok((sup::render_prompt(&v), provider, bindings.investigate.model.clone()))
    }

    /// Acts on the supervisor's route, only as far as the workflow allows.
    async fn route(
        &self,
        task_id: Uuid,
        target: Target,
        instruction: &str,
        title: Option<&str>,
        actor: &str,
    ) -> Routed {
        match self.route_inner(task_id, target, instruction, title, actor).await {
            Ok(r) => r,
            Err(e) => Routed {
                outcome: Some(format!("Nothing was routed: {}.", e.message.trim_end_matches('.'))),
                follow_up: None,
            },
        }
    }

    async fn route_inner(
        &self,
        task_id: Uuid,
        target: Target,
        instruction: &str,
        title: Option<&str>,
        actor: &str,
    ) -> Result<Routed, ProtocolError> {
        let (task, w, open) = self
            .storage
            .call(move |c| {
                let t = store::get_task(c, task_id)?;
                let w = wf_store::find_workflow(c, task_id)?;
                let open =
                    store::open_assignment(c, task_id)?.filter(|a| a.status == AssignmentStatus::Active);
                Ok((t, w, open))
            })
            .await
            .map_err(errors::storage)?;
        let note = format!("From the operator, through the task supervisor:\n\n{instruction}");
        let delivered_task = task.status == TaskStatus::Completed
            || w.as_ref().is_some_and(|w| w.status == WorkflowStatus::Delivered.as_str());
        if delivered_task {
            return self.follow_up(&task, target, instruction, title, actor).await;
        }
        let Some(rec) = w.filter(|w| w.definition == wf::DEFINITION) else {
            return Err(invalid_state("the task has no engineering workflow to route work to"));
        };
        let state = super::workflow::state_of(&rec).map_err(errors::storage)?;
        if wf::agent_busy(&state) {
            let current = state.current.clone();
            let same = current.as_ref().is_some_and(|s| Some(s.binding) == target.binding());
            if !same {
                let key = current.map(|s| s.key()).unwrap_or_default();
                return Err(invalid_state(format!(
                    "{key} is still working; ask again when it reports, or give the instruction to that agent"
                )));
            }
            let sent = self.note(task_id, note, actor.to_owned()).await?;
            let key = current.map(|s| s.key()).unwrap_or_default();
            let _ = open;
            return Ok(Routed {
                outcome: Some(if sent.delivered {
                    format!("Sent to the agent working on {key}.")
                } else {
                    format!(
                        "Saved for {key}: its agent is not running right now, so it gets the instruction on its next run."
                    )
                }),
                follow_up: None,
            });
        }
        // Check first, so nothing is written for a route the workflow refuses.
        wf::decide(&state, &Input::Directed { target }).map_err(|e| invalid_state(refusal(&e)))?;
        if target != Target::Tests {
            self.note(task_id, note, actor.to_owned()).await?;
        }
        self.apply_input(task_id, Input::Directed { target }, None, actor.to_owned()).await?;
        let after = self.workflow_required(task_id).await?;
        Ok(Routed {
            outcome: Some(match target {
                Target::Tests => "Switchyard is running the tests again.".to_owned(),
                _ => format!(
                    "Started {} with the instruction.",
                    after.step_key.as_deref().unwrap_or(target.as_str())
                ),
            }),
            follow_up: None,
        })
    }

    /// Work on a delivered task: a new task that links back, with its own
    /// engineering workflow (it starts from the delivered branch's state).
    async fn follow_up(
        &self,
        task: &store::TaskRecord,
        target: Target,
        instruction: &str,
        title: Option<&str>,
        actor: &str,
    ) -> Result<Routed, ProtocolError> {
        let title = title
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Follow-up: {}", task.title))
            .chars()
            .take(200)
            .collect::<String>();
        let created = self
            .create(TasksCreateParams {
                project_id: task.project_id,
                title,
                description: Some(format!(
                    "Follow-up of the delivered task \"{}\" ({}), requested through its supervisor.\n\n{instruction}",
                    task.title, task.id
                )),
                kind: Some(TaskKind::Implementation),
                priority: None,
                acceptance_criteria: vec![],
                depends_on: vec![],
                parent_task_id: None,
                publish: true,
            })
            .await?;
        let started = self
            .start_workflow(
                WorkflowsStartParams {
                    task_id: created.id,
                    overrides: RoleBindingsPatch::default(),
                    workspace_id: None,
                    definition: None,
                    lead: None,
                },
                actor.to_owned(),
            )
            .await;
        let _ = target;
        Ok(Routed {
            outcome: Some(match started {
                Ok(_) => format!(
                    "The task is delivered, so this became a follow-up task, \"{}\"; its workflow started.",
                    created.title
                ),
                Err(e) => format!(
                    "Opened the follow-up task \"{}\"; its workflow did not start: {}",
                    created.title, e.message
                ),
            }),
            follow_up: Some(created.id),
        })
    }

    /// After a Core restart: questions still waiting for an answer are
    /// recorded as unanswered (their session was stopped with Core).
    pub(super) async fn recover_supervisors(&self) {
        let tasks = match self.storage.call(|c| sup_store::tasks_thinking(c)).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(?e, "cannot read the supervisors' open questions");
                return;
            }
        };
        for task_id in tasks {
            let _ = self
                .record_reply(
                    task_id,
                    "core",
                    "No answer: Core restarted while the supervisor was answering. Ask again.".into(),
                    None,
                    None,
                    None,
                    Routed { outcome: None, follow_up: None },
                )
                .await;
        }
    }
}

fn target_name(b: wf::Binding) -> &'static str {
    match b {
        wf::Binding::Investigate => "reasoning",
        wf::Binding::Implement | wf::Binding::Correct => "coding",
        wf::Binding::Review => "review",
    }
}

fn refusal(e: &WorkflowError) -> String {
    match e {
        WorkflowError::NotAllowed(s) => format!("the workflow is {s}"),
        other => other.to_string(),
    }
}
