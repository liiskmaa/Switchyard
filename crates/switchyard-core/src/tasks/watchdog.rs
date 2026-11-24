//! The stuck-work watchdog (ADR 0045): every [`INTERVAL`], for each active
//! workflow and team, Core builds a [`Snapshot`] from its own records, asks
//! the pure [`check`] whether it is stuck, journals every change
//! (`task.stuck_changed`), and performs only the safe remedies itself:
//!
//! * an open workflow step whose agent session is gone is cancelled (as a
//!   Core restart would leave it), so Resume runs it again;
//! * a team whose Resume keeps bouncing is handed to its lead, once per
//!   reason.
//!
//! Everything else is shown to the operator with what they can do.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use switchyard_events::{DomainEvent, EventFilter, now_ms};
use switchyard_protocol::model::StuckItem;
use switchyard_storage::organizations as org_store;
use switchyard_storage::tasks as store;
use switchyard_storage::workflows as wf_store;
use switchyard_storage::{Connection, StorageError};
use switchyard_tasks::AssignmentStatus;
use switchyard_tasks::watchdog::{Kind, OpenStep, Remedy, Resume, Snapshot, Stuck, check};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::TaskService;
use crate::journal::{commit_checked, draft};
use crate::state::wait_shutdown;

/// What the watchdog last found, per task (shown by `watchdog.list`).
#[derive(Default)]
pub(crate) struct Findings {
    stuck: HashMap<Uuid, StuckItem>,
    /// (task, reason) already handed to a team's lead.
    told: HashSet<(Uuid, String)>,
    /// Orphaned steps already cancelled.
    cancelled: HashSet<Uuid>,
    /// Teams whose goal check runs now (ADR 0046).
    pub(super) goal_running: HashSet<Uuid>,
    /// Teams whose last goal check gave no verdict: not again before this time.
    pub(super) goal_retry: HashMap<Uuid, i64>,
    /// Why a team's last goal check gave no verdict, and when.
    pub(super) goal_failed: HashMap<Uuid, (i64, String)>,
    /// Providers whose referee turn failed (a usage limit, say): not chosen
    /// as referee again before this time.
    pub(super) referee_unusable: HashMap<String, i64>,
}

pub(crate) type SharedFindings = Arc<Mutex<Findings>>;

struct Gathered {
    task_id: Uuid,
    project_id: Uuid,
    title: String,
    snapshot: Snapshot,
    /// The open step's assignment, for the orphan remedy.
    open_assignment: Option<Uuid>,
    open_session: Option<Uuid>,
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Resumes and the waits that followed them, from the task's journal.
fn resumes(
    c: &Connection,
    task: Uuid,
    kind: Kind,
    since_ms: i64,
) -> Result<(Vec<Resume>, Option<i64>), StorageError> {
    let types: Vec<String> = match kind {
        Kind::Workflow => vec!["workflow.resumed".into(), "workflow.waiting".into()],
        Kind::Team => vec!["organization.status_changed".into()],
    };
    let events = switchyard_storage::events::read(
        c,
        &EventFilter { task_id: Some(task), event_types: types, limit: 5000, ..EventFilter::default() },
    )?;
    let mut out: Vec<Resume> = Vec::new();
    let mut waiting_since = None;
    for e in events {
        let at = e.occurred_at_ms;
        let p = &e.payload;
        let (resumed, waiting) = match e.event_type.as_str() {
            "workflow.resumed" => (true, None),
            "workflow.waiting" => (false, Some(str_of(p, "reason").unwrap_or_default().to_owned())),
            _ => match str_of(p, "status") {
                Some("running") => (true, None),
                Some("waiting") => (false, Some(str_of(p, "reason").unwrap_or_default().to_owned())),
                _ => (false, None),
            },
        };
        if let Some(reason) = waiting {
            waiting_since = Some(at);
            if let Some(last) = out.last_mut()
                && last.waiting_again.is_none()
            {
                last.waiting_again = Some((at, reason));
            }
        } else if resumed {
            waiting_since = None;
            if at >= since_ms {
                out.push(Resume { at_ms: at, waiting_again: None });
            }
        }
    }
    Ok((out, waiting_since))
}

fn oldest_pending_approval(c: &Connection, sessions: &[Uuid]) -> Result<Option<i64>, StorageError> {
    let mut oldest: Option<i64> = None;
    for s in sessions {
        for a in switchyard_storage::approvals::list(
            c,
            Some(switchyard_agents::ApprovalStatus::Pending),
            Some(*s),
            None,
            50,
        )? {
            oldest = Some(oldest.map_or(a.requested_at_ms, |o: i64| o.min(a.requested_at_ms)));
        }
    }
    Ok(oldest)
}

impl TaskService {
    /// Builds a snapshot of every active workflow and team.
    fn gather(&self, c: &Connection, now: i64) -> Result<Vec<Gathered>, StorageError> {
        let mut out = Vec::new();
        for w in wf_store::active_workflows(c)? {
            let task = store::get_task(c, w.task_id)?;
            let team = w.definition == switchyard_tasks::organization::DEFINITION;
            let kind = if team { Kind::Team } else { Kind::Workflow };
            let (status, reason, question, step_task, step_key, supervisor_session) = if team {
                let Some(rec) = org_store::find(c, w.task_id)? else { continue };
                let state: switchyard_tasks::organization::OrgState =
                    serde_json::from_value(rec.state.clone())
                        .map_err(|e| StorageError::Corrupt(e.to_string()))?;
                let (step_task, key, sup) = match &state.activity {
                    switchyard_tasks::organization::Activity::Working { task, .. } => {
                        (state.task(task).and_then(|t| t.task_id), Some(task.clone()), None)
                    }
                    switchyard_tasks::organization::Activity::Supervising { checkpoint } => (
                        None,
                        Some(format!("{} (checking)", checkpoint.supervisor)),
                        state.sessions.get(&checkpoint.supervisor).copied(),
                    ),
                    switchyard_tasks::organization::Activity::Idle => (None, None, None),
                };
                (rec.status.clone(), rec.reason.clone(), state.question_from.is_some(), step_task, key, sup)
            } else {
                let state: switchyard_tasks::workflow::WorkflowState =
                    serde_json::from_value(w.state.clone())
                        .map_err(|e| StorageError::Corrupt(e.to_string()))?;
                let question =
                    matches!(state.on_resume, Some(switchyard_tasks::workflow::Resume::Answer { .. }));
                (w.status.clone(), w.reason.clone(), question, Some(w.task_id), w.step_key.clone(), None)
            };
            // The open step: its assignment and session (a team's worker or its checking supervisor).
            let mut open_step = None;
            let mut open_assignment = None;
            let mut open_session = None;
            let mut sessions_of_task: Vec<Uuid> = Vec::new();
            if let Some(t) = step_task
                && let Some(a) = store::open_assignment(c, t)?
                && a.status == AssignmentStatus::Active
            {
                open_assignment = Some(a.id);
                open_session = a.session_id;
                let last = match a.session_id {
                    Some(s) => Some(switchyard_storage::sessions::get(c, s)?.last_activity_at_ms),
                    None => None,
                };
                open_step = Some((
                    a.step_key.clone().or(step_key.clone()).unwrap_or_default(),
                    a.started_at_ms.unwrap_or(a.created_at_ms),
                    last,
                ));
            } else if let Some(s) = supervisor_session {
                let rec = switchyard_storage::sessions::get(c, s)?;
                open_session = Some(s);
                open_step = Some((
                    step_key.clone().unwrap_or_default(),
                    rec.started_at_ms,
                    Some(rec.last_activity_at_ms),
                ));
            }
            if let Some(s) = open_session {
                sessions_of_task.push(s);
            }
            let (resumes, waiting_since) =
                resumes(c, w.task_id, kind, now - self.watchdog_limits.bounce_lookback_ms)?;
            let alive = open_session.is_some_and(|s| self.agents.is_attached(s));
            out.push(Gathered {
                task_id: w.task_id,
                project_id: task.project_id,
                title: task.title.clone(),
                snapshot: Snapshot {
                    kind,
                    status,
                    reason,
                    waiting_since_ms: waiting_since,
                    question,
                    open_step: open_step.map(|(key, since, last)| OpenStep {
                        key,
                        session_alive: alive,
                        since_ms: since,
                        last_activity_ms: last,
                    }),
                    oldest_approval_ms: oldest_pending_approval(c, &sessions_of_task)?,
                    resumes,
                },
                open_assignment,
                open_session,
            });
        }
        Ok(out)
    }

    /// One watchdog pass.
    pub(crate) async fn watch_once(&self) {
        let now = now_ms();
        let svc = self.clone();
        let gathered = match self.storage.call(move |c| svc.gather(c, now)).await {
            Ok(g) => g,
            Err(err) => {
                tracing::warn!(?err, "watchdog cannot read the active work");
                return;
            }
        };
        let mut seen = HashSet::new();
        for g in gathered {
            seen.insert(g.task_id);
            // A team's goal check, when due (ADR 0046).
            if g.snapshot.kind == Kind::Team {
                let id = g.task_id;
                if let Ok(rec) = self.storage.call(move |c| org_store::get(c, id)).await
                    && let Ok(state) =
                        serde_json::from_value::<switchyard_tasks::organization::OrgState>(rec.state)
                {
                    self.maybe_check_goal(id, &state, now);
                }
            }
            let found = check(&g.snapshot, now, &self.watchdog_limits);
            let mut applied = None;
            if let Some(Stuck { remedy, .. }) = &found {
                applied = self.remedy(&g, remedy).await;
            }
            let item = found.map(|s| StuckItem {
                task_id: g.task_id,
                project_id: g.project_id,
                task_title: g.title.clone(),
                kind: match g.snapshot.kind {
                    Kind::Workflow => "workflow".into(),
                    Kind::Team => "team".into(),
                },
                code: s.code.as_str().into(),
                title: s.title,
                detail: s.detail,
                since_ms: s.since_ms,
                session_id: g.open_session,
                remedy_applied: applied,
            });
            self.record_stuck(g.task_id, item).await;
        }
        // Work that ended or is no longer active is not stuck any more.
        let gone: Vec<Uuid> = {
            let f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.stuck.keys().filter(|t| !seen.contains(t)).copied().collect()
        };
        for t in gone {
            self.record_stuck(t, None).await;
        }
    }

    /// Performs a safe remedy once; returns what was done.
    async fn remedy(&self, g: &Gathered, remedy: &Remedy) -> Option<String> {
        match remedy {
            Remedy::None => None,
            Remedy::InterruptStep(key) => {
                let a = g.open_assignment?;
                {
                    let mut f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !f.cancelled.insert(a) {
                        return None;
                    }
                }
                match self
                    .cancel_assignment(
                        a,
                        Some(format!("its agent session is gone (watchdog); {key} can run again")),
                        "watchdog".into(),
                    )
                    .await
                {
                    Ok(_) => Some(format!("Ended {key}, whose agent was gone; resume to run it again.")),
                    Err(err) => {
                        tracing::warn!(?err, task = %g.task_id, "watchdog could not end an orphaned step");
                        None
                    }
                }
            }
            Remedy::TellLead(reason) => {
                {
                    let mut f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !f.told.insert((g.task_id, reason.clone())) {
                        return Some("Handed to the lead.".into());
                    }
                }
                let text = format!(
                    "Switchyard's watchdog: resuming the team keeps stopping at the same place: \u{201c}{reason}\u{201d} \
                     Decide how to continue (cancel or re-plan what cannot run, or ask the user)."
                );
                let input = switchyard_tasks::organization::Input::Nudge { text, now_ms: now_ms() };
                match self.apply_org(g.task_id, input, None).await {
                    Ok(()) => Some("Handed to the lead.".into()),
                    Err(err) => {
                        tracing::warn!(?err, task = %g.task_id, "watchdog could not hand a stuck team to its lead");
                        None
                    }
                }
            }
        }
    }

    /// Stores the finding and journals a change (`task.stuck_changed`). A
    /// title that only counts on ("silent for 401 min", "… 402 min") is the
    /// same finding: it is kept up to date in memory but not journaled again.
    async fn record_stuck(&self, task: Uuid, item: Option<StuckItem>) {
        let changed = {
            let mut f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = |i: &StuckItem| (i.code.clone(), same_finding(&i.title), i.remedy_applied.clone());
            let before = f.stuck.get(&task).map(key);
            let after = item.as_ref().map(key);
            match &item {
                Some(i) => {
                    f.stuck.insert(task, i.clone());
                }
                None => {
                    f.stuck.remove(&task);
                }
            }
            before != after
        };
        if !changed {
            return;
        }
        let result = commit_checked(&self.storage, &self.bus, move |tx| {
            let Ok(t) = store::get_task(tx, task) else { return Ok(Ok(((), Vec::new()))) };
            let event = DomainEvent::TaskStuckChanged {
                code: item.as_ref().map(|i| i.code.clone()),
                title: item.as_ref().map(|i| i.title.clone()),
                remedy: item.as_ref().and_then(|i| i.remedy_applied.clone()),
            };
            Ok(Ok(((), vec![draft(event, &super::task_context(&t))?])))
        })
        .await;
        if let Err(err) = result {
            tracing::warn!(?err, %task, "watchdog could not journal a finding");
        }
    }

    /// What the watchdog currently finds stuck, newest first.
    pub fn stuck(&self) -> Vec<StuckItem> {
        let f = self.findings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v: Vec<StuckItem> = f.stuck.values().cloned().collect();
        v.sort_by_key(|i| std::cmp::Reverse(i.since_ms));
        v
    }
}

/// A finding's title without its counts (words that are only a number), so
/// a growing count is no change; a step key such as `org:review#4` stays.
fn same_finding(title: &str) -> String {
    title
        .split_whitespace()
        .map(|w| if w.chars().all(|c| c.is_ascii_digit()) { "N" } else { w })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn spawn_watchdog(
    svc: TaskService,
    interval: Duration,
    mut shutdown: watch::Receiver<Option<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = wait_shutdown(&mut shutdown) => break,
                () = tokio::time::sleep(interval) => svc.watch_once().await,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_growing_count_is_the_same_finding() {
        assert_eq!(
            super::same_finding("org:review#4 has been silent for 401 min"),
            super::same_finding("org:review#4 has been silent for 402 min")
        );
        assert_ne!(
            super::same_finding("org:review#4 has been silent for 9 min"),
            super::same_finding("org:review#5 has been silent for 9 min")
        );
    }
}
