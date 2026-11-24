//! The goal check (ADR 0046, stage 2): when a team's check is due, Core runs
//! an independent, read-only referee (a provider other than the lead's when
//! one is installed) in the task's side workspace, with the objective, the
//! team's tasks and Core's evidence (the Git changes), parses its verdict and
//! gives it to the organization (`Input::GoalChecked`). The verdict only
//! steers; nothing is approved or completed from it.

use std::time::Duration;

use switchyard_agents::{AgentProvider, SessionAccess};
use switchyard_events::{EventFilter, now_ms};
use switchyard_protocol::model::GoalKeeperInfo;
use switchyard_storage::organizations as org_store;
use switchyard_tasks::organization::{self as org, Input, OrgState};
use uuid::Uuid;

use super::TaskService;
use crate::handlers::errors;

/// A check runs at most this long.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How much of the diff the referee sees.
const MAX_DIFF: usize = 30_000;
/// A team is checked at least this often while it works.
pub(crate) const INTERVAL_MS: i64 = 30 * 60_000;
/// A provider whose referee turn failed is not chosen again for this long.
const REFEREE_PAUSE_MS: i64 = 60 * 60_000;
/// After a check that gave no usable verdict, wait this long before trying again.
const RETRY_MS: i64 = 10 * 60_000;

impl TaskService {
    /// Starts a goal check for the team on `task_id` if one is due and none runs.
    pub(super) fn maybe_check_goal(&self, task_id: Uuid, state: &OrgState, now: i64) {
        if !org::goal_check_due(state, now, INTERVAL_MS) {
            return;
        }
        {
            let mut f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if f.goal_running.contains(&task_id) || f.goal_retry.get(&task_id).is_some_and(|t| now < *t) {
                return;
            }
            f.goal_running.insert(task_id);
        }
        let svc = self.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(TIMEOUT, svc.check_goal(task_id)).await;
            let failed = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => {
                    tracing::warn!(%task_id, error = %e, "goal check gave no verdict");
                    Some(e)
                }
                Err(_) => {
                    tracing::warn!(%task_id, "goal check timed out");
                    Some("the check took longer than 10 minutes".to_owned())
                }
            };
            let mut f = svc.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.goal_running.remove(&task_id);
            match failed {
                Some(e) => {
                    f.goal_retry.insert(task_id, now_ms() + RETRY_MS);
                    f.goal_failed.insert(task_id, (now_ms(), e));
                }
                None => {
                    f.goal_failed.remove(&task_id);
                }
            }
        });
    }

    /// The goal keeper's state for the team page: who would check, whether a
    /// check runs, when the next is due, and why the last one failed.
    pub(super) async fn goal_keeper(&self, task_id: Uuid, state: &OrgState) -> GoalKeeperInfo {
        let lead = state
            .agents
            .iter()
            .find(|a| a.reports_to.is_none())
            .map(|a| a.provider.clone())
            .unwrap_or_default();
        let referee = self.referee(&lead).await.map(|(_, by)| by);
        let now = now_ms();
        let (checking, retry, failed) = {
            let f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                f.goal_running.contains(&task_id),
                f.goal_retry.get(&task_id).copied(),
                f.goal_failed.get(&task_id).cloned(),
            )
        };
        let running = state.status == org::OrgStatus::Running;
        let interval = state.last_goal_check_ms.max(state.started_at_ms) + INTERVAL_MS;
        let next_check_ms = running.then(|| {
            let due_at = if state.goal_due { now } else { interval };
            retry.map_or(due_at, |r| due_at.max(r))
        });
        GoalKeeperInfo {
            referee,
            checking,
            due: running && org::goal_check_due(state, now, INTERVAL_MS),
            next_check_ms,
            last_error: failed.as_ref().map(|(_, e)| e.clone()),
            last_error_ms: failed.map(|(at, _)| at),
        }
    }

    /// The referee: not the lead's provider when another one is available.
    async fn referee(&self, lead: &str) -> Option<(AgentProvider, String)> {
        let mut fallback = None;
        let paused: Vec<String> = {
            let now = now_ms();
            let f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.referee_unusable.iter().filter(|(_, until)| now < **until).map(|(p, _)| p.clone()).collect()
        };
        for p in [AgentProvider::Gemini, AgentProvider::Codex, AgentProvider::ClaudeCode] {
            if paused.iter().any(|x| x == p.as_str()) {
                continue;
            }
            let a = self.agents.availability(p, false).await;
            if !(a.installed && a.error.is_none()) {
                continue;
            }
            if p.as_str() != lead {
                return Some((p, switchyard_agents::display_name(p).to_owned()));
            }
            fallback = Some((
                p,
                format!(
                    "{} (the lead's own provider: no other is installed)",
                    switchyard_agents::display_name(p)
                ),
            ));
        }
        fallback
    }

    async fn check_goal(&self, task_id: Uuid) -> Result<(), String> {
        let (rec, state, decisions) = self
            .storage
            .call(move |c| {
                let rec = org_store::get(c, task_id)?;
                let state: OrgState = serde_json::from_value(rec.state.clone())
                    .map_err(|e| switchyard_storage::StorageError::Corrupt(e.to_string()))?;
                let decisions: Vec<String> = org_store::context(c, task_id)?
                    .into_iter()
                    .rev()
                    .filter(|e| {
                        matches!(e.kind.as_str(), "decision" | "progress" | "deliverable")
                            && e.status != "superseded"
                    })
                    .take(10)
                    .map(|e| e.text.chars().take(400).collect())
                    .collect();
                Ok((rec, state, decisions))
            })
            .await
            .map_err(|e| errors::storage(e).message)?;
        let lead = state
            .agents
            .iter()
            .find(|a| a.reports_to.is_none())
            .map(|a| a.provider.clone())
            .unwrap_or_default();
        let (provider, by) = self.referee(&lead).await.ok_or("no agent is available for the goal check")?;
        let changes = match self.workspaces.record(rec.workspace_id).await {
            Ok(w) => match self.workspaces.changes_text(&w, MAX_DIFF).await {
                Ok(Some(t)) => t,
                Ok(None) => "No changes yet.".into(),
                Err(e) => format!("Could not read the changes: {e}"),
            },
            Err(e) => format!("Could not read the workspace: {}", e.message),
        };
        let prompt =
            format!("{}\nTask id: {task_id}\n", org::goal::render_prompt(&state, &changes, &decisions));
        // Always a fresh read-only session: the referee must not carry the
        // supervisor's conversation, nor replace it.
        let (workspace, project_id) = self.side_workspace(task_id).await.map_err(|e| e.message)?;
        let after = self
            .storage
            .call(|c| Ok(switchyard_storage::events::last_seq(c)?.unwrap_or(0)))
            .await
            .map_err(|e| errors::storage(e).message)?;
        let session_id = self
            .agents
            .start(provider, SessionAccess::Review, workspace, Some(project_id), None, None)
            .await
            .map_err(|e| e.message)?
            .id;
        let filter = EventFilter {
            event_types: vec!["agent.turn_completed".into(), "agent.exited".into(), "agent.failed".into()],
            ..EventFilter::default()
        };
        let mut sub = self.bus.subscribe(Some(after), filter);
        if let Err(e) = self.agents.send(session_id, prompt, false).await {
            self.stop_session(session_id).await;
            return Err(e.message);
        }
        let ended = loop {
            match sub.next().await {
                Ok(e) if e.context.session_id == Some(session_id) => break Some(e),
                Ok(_) => {}
                Err(switchyard_events::RecvError::Closed) => break None,
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        };
        let text = match ended {
            Some(e) => self.supervisor_turn_text(session_id, after, &e).await,
            None => Err("Core is stopping".into()),
        };
        self.stop_session(session_id).await;
        let text = match text {
            Ok(t) => t,
            Err(e) => {
                // The agent itself failed: the next check asks another one.
                self.findings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .referee_unusable
                    .insert(provider.as_str().to_owned(), now_ms() + REFEREE_PAUSE_MS);
                return Err(format!("{by}: {e}"));
            }
        };
        let mut report = org::goal::parse_verdict(&text, &state)?;
        report.by = by;
        self.apply_org(task_id, Input::GoalChecked { report, now_ms: now_ms() }, None)
            .await
            .map_err(|e| e.message)
    }
}
