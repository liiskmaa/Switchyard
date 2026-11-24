//! Starting a cancelled or failed task again (`tasks.start_again`): a new
//! task with the same title, description and criteria, and its workflow or
//! team. By default it continues in the old task's workspace, so the work
//! already done stays owned by a live task instead of being orphaned. The
//! link between the two is derived from the shared workspace
//! (`WorkflowInfo.continues_task_id` / `continued_by_task_id`).

use switchyard_protocol::methods::{
    TasksCreateParams, TasksStartAgainParams, TasksStartAgainResult, WorkflowsStartParams,
};
use switchyard_protocol::model::RoleBindingsPatch;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::organizations as org_store;
use switchyard_storage::tasks as store;
use switchyard_storage::workflows as wf_store;
use switchyard_tasks::TaskStatus;
use switchyard_tasks::organization::{self as org, OrgState, TaskState};
use uuid::Uuid;

use super::TaskService;
use crate::handlers::errors;

const MAX_DESCRIPTION: usize = 20_000;

impl TaskService {
    /// Cancels a task whose workflow ended, and its open subtasks.
    pub(super) async fn close_ended(&self, task_id: Uuid, reason: &str, actor: &str) {
        let open = self
            .storage
            .call(move |c| Ok(!store::get_task(c, task_id)?.status.is_terminal()))
            .await
            .unwrap_or(false);
        if open
            && let Err(e) =
                self.close(task_id, TaskStatus::Cancelled, reason.to_owned(), actor.to_owned()).await
        {
            tracing::warn!(%task_id, error = %e.message, "could not cancel the task of an ended workflow");
        }
        self.close_subtasks(task_id, reason, actor).await;
    }

    /// Cancels every open task below `root`: its child tasks and, for a
    /// team's objective, the team's tasks, at every depth (deepest first).
    /// Nothing is deleted; cancelled tasks keep their history.
    pub(super) async fn close_subtasks(&self, root: Uuid, reason: &str, actor: &str) {
        let open: Vec<Uuid> = self
            .storage
            .call(move |c| {
                let mut seen = std::collections::HashSet::from([root]);
                let mut order = Vec::new();
                let mut stack = vec![root];
                while let Some(parent) = stack.pop() {
                    let mut below = store::child_task_ids(c, parent)?;
                    if let Some(rec) = org_store::find(c, parent)?
                        && let Ok(state) = serde_json::from_value::<OrgState>(rec.state)
                    {
                        below.extend(state.tasks.iter().filter_map(|t| t.task_id));
                    }
                    for id in below {
                        if seen.insert(id) {
                            order.push(id);
                            stack.push(id);
                        }
                    }
                }
                let mut out = Vec::new();
                for id in order.into_iter().rev() {
                    if !store::get_task(c, id)?.status.is_terminal() {
                        out.push(id);
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        for id in open {
            let why = format!("its parent task was cancelled: {reason}");
            if let Err(e) = self.close(id, TaskStatus::Cancelled, why, actor.to_owned()).await {
                tracing::warn!(task_id = %id, error = %e.message, "could not cancel a team subtask");
            }
        }
    }

    pub async fn start_again(
        &self,
        p: TasksStartAgainParams,
        actor: String,
    ) -> Result<TasksStartAgainResult, ProtocolError> {
        let id = p.task_id;
        let (task, workflow, team, in_team) = self
            .storage
            .call(move |c| {
                let t = store::get_task(c, id)?;
                let w = wf_store::find_workflow(c, id)?;
                let o = org_store::find(c, id)?;
                let active = wf_store::active_workflows(c)?;
                // Part of a team: the parent's organization lists it.
                let in_team = match t.parent_task_id {
                    Some(parent) => org_store::find(c, parent)?
                        .and_then(|r| serde_json::from_value::<OrgState>(r.state).ok())
                        .is_some_and(|s| s.tasks.iter().any(|x| x.task_id == Some(id))),
                    None => false,
                };
                Ok((t, w.map(|w| (w, active)), o, in_team))
            })
            .await
            .map_err(errors::storage)?;
        let workflow_ended =
            workflow.as_ref().is_some_and(|(w, _)| matches!(w.status.as_str(), "cancelled" | "failed"));
        if !matches!(task.status, TaskStatus::Cancelled | TaskStatus::Failed) && !workflow_ended {
            return Err(invalid_state(format!(
                "only a cancelled or failed task starts again; this one is {}",
                task.status
            )));
        }
        // A task left open by a workflow that ended earlier is closed first.
        self.close_ended(id, "its workflow had ended; started again", &actor).await;
        let task = self.storage.call(move |c| store::get_task(c, id)).await.map_err(errors::storage)?;
        if in_team {
            return Err(invalid_state("this is part of a team's work: start the team's objective again"));
        }
        let workspace = if p.keep_work {
            let (w, active) = workflow.as_ref().ok_or_else(|| {
                invalid_state("the task has no workspace to continue; start it from scratch")
            })?;
            let ws = w.workspace_id.ok_or_else(|| {
                invalid_state("the task has no workspace to continue; start it from scratch")
            })?;
            if let Some(other) = active.iter().find(|a| a.workspace_id == Some(ws)) {
                return Err(ProtocolError::new(
                    ErrorCode::Conflict,
                    format!("task {} already continues this work", other.task_id),
                ));
            }
            self.workspaces.record(ws).await.map_err(|e| {
                invalid_state(format!("the task's workspace is gone ({}); start it from scratch", e.message))
            })?;
            let open = self
                .integrations
                .list(Some(task.project_id), None, false, 200)
                .await?
                .into_iter()
                .find(|i| i.workspace_id == ws);
            if let Some(i) = open {
                return Err(invalid_state(format!(
                    "the workspace has an open integration ({}); finish or cancel it first",
                    i.status
                )));
            }
            Some(ws)
        } else {
            None
        };
        let definition = p
            .definition
            .clone()
            .or_else(|| workflow.as_ref().map(|(w, _)| w.definition.clone()))
            .filter(|d| d == org::DEFINITION);
        let state: Option<OrgState> = team.and_then(|r| serde_json::from_value(r.state).ok());
        let description = describe(&task, state.as_ref(), workspace.is_some());
        let created = self
            .create(TasksCreateParams {
                project_id: task.project_id,
                title: task.title.clone(),
                description: Some(description),
                kind: Some(task.kind),
                priority: Some(task.priority),
                acceptance_criteria: task.acceptance_criteria.clone(),
                depends_on: vec![],
                // It stays where the old one was (a subtask stays a subtask).
                parent_task_id: task.parent_task_id,
                publish: true,
            })
            .await?;
        let started = self
            .start_workflow(
                WorkflowsStartParams {
                    task_id: created.id,
                    overrides: RoleBindingsPatch::default(),
                    workspace_id: workspace,
                    definition,
                    lead: p.lead,
                },
                actor,
            )
            .await;
        let (workflow, start_error) = match started {
            Ok(w) => (Some(w), None),
            Err(e) => (None, Some(e.message)),
        };
        let task = self.get(created.id).await.map(|d| d.task).unwrap_or(created);
        Ok(TasksStartAgainResult { task, workflow, start_error })
    }
}

fn invalid_state(m: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidState, m.into())
}

/// The new task's description: the original, then what the earlier attempt
/// left (its agents' claims, marked as such).
fn describe(task: &store::TaskRecord, state: Option<&OrgState>, kept: bool) -> String {
    let mut d = task.description.clone().unwrap_or_default();
    d.push_str(&format!(
        "\n\n---\nStarted again from the {} task \"{}\" ({}).",
        task.status, task.title, task.id
    ));
    if kept {
        d.push_str(
            " This workspace already contains that task's work. Look at what is there first and continue it; do \
             not redo what is done.",
        );
    } else {
        d.push_str(" It starts from the project's current branch; the earlier work is not here.");
    }
    if let Some(s) = state {
        let done: Vec<String> = s
            .tasks
            .iter()
            .filter(|t| t.key != org::ROOT && t.state == TaskState::Accepted)
            .map(|t| {
                format!(
                    "- {}{}",
                    t.title,
                    t.summary
                        .as_deref()
                        .map(|x| format!(": {}", x.chars().take(300).collect::<String>()))
                        .unwrap_or_default()
                )
            })
            .collect();
        if !done.is_empty() {
            d.push_str("\n\nAccepted in the earlier attempt (its agents' claims, check them in the code):\n");
            d.push_str(&done.join("\n"));
        }
        if let Some(g) = &s.goal {
            d.push_str(&format!("\n\nThe last goal check said ({}): {}", g.verdict.as_str(), g.summary));
        }
        if let Some(r) = s.reason.as_deref().or(s.final_summary.as_deref()) {
            d.push_str(&format!("\n\nWhere it stopped: {}", r.chars().take(600).collect::<String>()));
        }
    } else if let Some(f) = &task.failure {
        d.push_str(&format!("\n\nWhere it stopped: {f}"));
    }
    d.chars().take(MAX_DESCRIPTION).collect()
}
