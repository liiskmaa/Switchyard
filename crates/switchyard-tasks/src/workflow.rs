//! The built-in engineering workflow `engineering@1` (milestone 8.1, ADR 0033),
//! as a pure decision function.
//!
//! A workflow drives one task through investigation, implementation, review
//! and bounded correction rounds, and stops at the human delivery gate. Core
//! stores a [`WorkflowState`], turns task events into an [`Input`], calls
//! [`decide`], commits the new state together with its journal events and the
//! next assignment, and only then performs the [`Action`].
//!
//! Every assignment the workflow creates has a stable step key:
//! `investigate#1`, `implement#1`, `review#1`, `implement#2` (the first
//! correction), … A retry of a step that failed appends the attempt:
//! `implement#1@2`. Core stores the key with a unique index, so replaying an
//! event can never create the same step twice.
//!
//! Nothing here performs I/O or knows about providers.

use serde::{Deserialize, Serialize};

use crate::model::{ReviewResult, Role};

pub const DEFINITION: &str = "engineering";
pub const VERSION: u32 = 1;
/// Automatic correction rounds after a review requests changes.
pub const MAX_CORRECTIONS: u32 = 2;
/// Automatic repair notes for a turn without a usable report.
pub const MAX_REPAIRS: u32 = 2;
/// Automatic retries of a step whose session failed.
pub const MAX_RETRIES: u32 = 1;

/// Which role binding (provider and model) a step uses. A correction is an
/// implement assignment with its own binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Investigate,
    Implement,
    Review,
    Correct,
}

impl Binding {
    pub const ALL: [Binding; 4] =
        [Binding::Investigate, Binding::Implement, Binding::Review, Binding::Correct];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Investigate => "investigate",
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Correct => "correct",
        }
    }

    /// The assignment role a step with this binding runs as.
    pub fn role(self) -> Role {
        match self {
            Self::Investigate => Role::Investigate,
            Self::Implement | Self::Correct => Role::Implement,
            Self::Review => Role::Review,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Investigation,
    Implementation,
    Review,
    Correction,
    /// Reached the human delivery gate (integration and completion are the operator's).
    Delivery,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Investigation => "investigation",
            Self::Implementation => "implementation",
            Self::Review => "review",
            Self::Correction => "correction",
            Self::Delivery => "delivery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Created, no step started yet.
    #[default]
    Pending,
    /// A step is running or about to start; no human action is needed.
    Running,
    /// Stopped for a human: `reason` says why, `on_resume` what resuming does.
    Waiting,
    /// The operator paused it: the current step finishes, nothing new starts.
    Paused,
    /// The review approved: ready for human-approved delivery.
    Completed,
    Cancelled,
    /// The task failed or was closed while the workflow was not done.
    Failed,
}

impl WorkflowStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn is_final(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "waiting" => Self::Waiting,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// One step of the workflow: one assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub binding: Binding,
    pub stage: Stage,
    /// Investigate: always 1. Implement/review: the round (correction n is implement n+1).
    pub round: u32,
    /// 1 for the first run of the step, 2 for its retry, …
    pub attempt: u32,
}

impl Step {
    fn new(binding: Binding, stage: Stage, round: u32) -> Self {
        Self { binding, stage, round, attempt: 1 }
    }

    /// The unique step key, e.g. `implement#2` or `review#1@2`.
    pub fn key(&self) -> String {
        let base = format!("{}#{}", self.binding.role().as_str(), self.round);
        if self.attempt > 1 { format!("{base}@{}", self.attempt) } else { base }
    }

    pub fn role(&self) -> Role {
        self.binding.role()
    }

    fn retry(&self) -> Self {
        Self { attempt: self.attempt + 1, ..self.clone() }
    }

    /// Correction rounds before this step.
    pub fn corrections(&self) -> u32 {
        match self.binding {
            Binding::Investigate => 0,
            _ => self.round.saturating_sub(1),
        }
    }
}

/// What resuming a waiting or paused workflow does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Resume {
    /// The current assignment is still open (an agent's question, or a
    /// paused step still running): just continue.
    Continue,
    /// Send the current assignment another repair note.
    Repair,
    /// Start this step.
    Start { step: Step },
}

/// The durable state of one workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowState {
    pub status: WorkflowStatus,
    pub stage: Option<Stage>,
    /// The step whose assignment is (or was last) current.
    pub current: Option<Step>,
    /// Repair notes sent to the current assignment.
    pub repairs: u32,
    /// Why the workflow waits, stopped or finished.
    pub reason: Option<String>,
    pub on_resume: Option<Resume>,
}

/// How a step's assignment ended with a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Findings, a proposal or an implementation report: the step is done.
    Done,
    Review(ReviewResult),
    /// The agent cannot continue.
    Blocker(String),
}

impl Outcome {
    /// From an assignment's recorded outcome (`reported: findings`,
    /// `review: changes_requested`, `blocker`, …). `None` if it is not a report.
    pub fn from_assignment(outcome: &str, summary: Option<&str>) -> Option<Self> {
        if let Some(result) = outcome.strip_prefix("review: ") {
            let r = match result {
                "approved" => ReviewResult::Approved,
                "changes_requested" => ReviewResult::ChangesRequested,
                "rejected" => ReviewResult::Rejected,
                _ => ReviewResult::Blocked,
            };
            return Some(Self::Review(r));
        }
        if outcome == "blocker" {
            return Some(Self::Blocker(summary.unwrap_or("no reason given").to_owned()));
        }
        outcome.starts_with("reported: ").then_some(Self::Done)
    }
}

/// Something that happened, as the workflow sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Begin the workflow.
    Start,
    /// The step's assignment reported.
    Reported {
        step: String,
        outcome: Outcome,
    },
    /// A turn of the step's assignment ended without a usable report; the
    /// assignment stays open.
    Unusable {
        step: String,
        error: String,
    },
    /// The step's agent asked the operator a question; the assignment stays open.
    Question {
        step: String,
        summary: String,
    },
    /// The step's assignment ended without a report.
    Ended {
        step: String,
        cancelled: bool,
        reason: String,
    },
    /// Core restarted while the step's session was running.
    Interrupted {
        step: String,
    },
    Pause,
    Resume,
    Cancel {
        reason: String,
    },
    /// The task was completed, failed or cancelled by the operator.
    TaskClosed {
        status: String,
    },
}

/// What Core must do after committing the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Create and start this step's assignment (created in the same
    /// transaction as the state, with the step key).
    Start(Step),
    /// Send the current assignment a repair note (`n` of [`MAX_REPAIRS`]).
    Repair {
        n: u32,
    },
    /// Cancel the current assignment, if it is open.
    CancelAssignment,
}

/// A journal-worthy change the decision made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Started,
    StageChanged(Step),
    Waiting(String),
    Paused,
    Resumed,
    Completed(String),
    Cancelled(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub state: WorkflowState,
    pub action: Action,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    #[error("the workflow is {0}")]
    NotAllowed(&'static str),
}

/// Decides what happens next. Inputs about a step other than the current
/// one, or arriving after the workflow ended, change nothing (replays and
/// stale events are harmless).
pub fn decide(state: &WorkflowState, input: &Input) -> Result<Decision, WorkflowError> {
    let mut d = Decision { state: state.clone(), action: Action::None, changes: Vec::new() };
    let status = state.status;
    match input {
        Input::Start => {
            if status != WorkflowStatus::Pending {
                return Err(WorkflowError::NotAllowed(status.as_str()));
            }
            d.changes.push(Change::Started);
            begin(&mut d, Step::new(Binding::Investigate, Stage::Investigation, 1));
        }
        Input::Pause => match status {
            WorkflowStatus::Running => {
                d.state.status = WorkflowStatus::Paused;
                d.state.on_resume = Some(Resume::Continue);
                d.state.reason = Some("paused by the operator".into());
                d.changes.push(Change::Paused);
            }
            s => return Err(WorkflowError::NotAllowed(s.as_str())),
        },
        Input::Resume => match status {
            WorkflowStatus::Waiting | WorkflowStatus::Paused => {
                d.changes.push(Change::Resumed);
                match state.on_resume.clone().unwrap_or(Resume::Continue) {
                    Resume::Continue => run(&mut d),
                    Resume::Repair => {
                        run(&mut d);
                        d.state.repairs = 1;
                        d.action = Action::Repair { n: 1 };
                    }
                    Resume::Start { step } => begin(&mut d, step),
                }
            }
            s => return Err(WorkflowError::NotAllowed(s.as_str())),
        },
        Input::Cancel { reason } => {
            if status.is_final() {
                return Err(WorkflowError::NotAllowed(status.as_str()));
            }
            finish(&mut d, WorkflowStatus::Cancelled, reason.clone());
            d.changes.push(Change::Cancelled(reason.clone()));
            d.action = Action::CancelAssignment;
        }
        Input::TaskClosed { status: task_status } => {
            if status.is_final() {
                return Ok(d);
            }
            let reason = format!("the task was {task_status} by the operator");
            if task_status == "failed" {
                finish(&mut d, WorkflowStatus::Failed, reason.clone());
                d.changes.push(Change::Failed(reason));
            } else {
                finish(&mut d, WorkflowStatus::Cancelled, reason.clone());
                d.changes.push(Change::Cancelled(reason));
            }
            d.action = Action::CancelAssignment;
        }
        Input::Reported { step, outcome } => {
            let Some(current) = current_for(state, step) else { return Ok(d) };
            reported(&mut d, &current, outcome);
        }
        Input::Unusable { step, error } => {
            if current_for(state, step).is_none() {
                return Ok(d);
            }
            if state.repairs < MAX_REPAIRS {
                d.state.repairs += 1;
                d.action = Action::Repair { n: d.state.repairs };
            } else {
                wait(
                    &mut d,
                    format!(
                        "{step} still did not end with a usable report after {MAX_REPAIRS} repair notes ({error}). \
                         Send the agent a note, resume to send another repair note, or cancel."
                    ),
                    Resume::Repair,
                );
            }
        }
        Input::Question { step, summary } => {
            if current_for(state, step).is_none() {
                return Ok(d);
            }
            wait(
                &mut d,
                format!(
                    "{step} asks you: {summary}. Answer with a note; the workflow continues when it reports."
                ),
                Resume::Continue,
            );
        }
        Input::Ended { step, cancelled, reason } => {
            let Some(current) = current_for(state, step) else { return Ok(d) };
            if *cancelled {
                wait(
                    &mut d,
                    format!("{step} was cancelled ({reason}). Resume to run it again."),
                    Resume::Start { step: current.retry() },
                );
            } else if current.attempt <= MAX_RETRIES {
                let next = current.retry();
                if !start_or_hold(&mut d, next) {
                    d.state.reason =
                        Some(format!("{step} ended without a report ({reason}); its retry waits"));
                }
            } else {
                wait(
                    &mut d,
                    format!(
                        "{step} ended without a report again ({reason}); it was already retried. Resume to try once \
                         more, or cancel."
                    ),
                    Resume::Start { step: current.retry() },
                );
            }
        }
        Input::Interrupted { step } => {
            let Some(current) = current_for(state, step) else { return Ok(d) };
            wait(
                &mut d,
                format!("Core restarted while {step} was running. Resume to run it again."),
                Resume::Start { step: current.retry() },
            );
        }
    }
    Ok(d)
}

/// The current step, if `key` names it and the workflow still follows it.
/// While waiting to start a fresh attempt, the old step is over: late
/// events about it (for example a failure replayed after a Core restart)
/// change nothing.
fn current_for(state: &WorkflowState, key: &str) -> Option<Step> {
    if state.status.is_final() || state.status == WorkflowStatus::Pending {
        return None;
    }
    if state.status == WorkflowStatus::Waiting && matches!(state.on_resume, Some(Resume::Start { .. })) {
        return None;
    }
    state.current.clone().filter(|s| s.key() == key)
}

fn reported(d: &mut Decision, current: &Step, outcome: &Outcome) {
    let key = current.key();
    match (current.binding, outcome) {
        (_, Outcome::Blocker(why)) => {
            // A blocked review is re-run; blocked work is retried, both on the operator's word.
            wait(d, format!("{key} reported a blocker: {why}"), Resume::Start { step: current.retry() });
        }
        (Binding::Investigate, _) => {
            start_or_hold(d, Step::new(Binding::Implement, Stage::Implementation, 1));
        }
        (Binding::Implement | Binding::Correct, _) => {
            start_or_hold(d, Step::new(Binding::Review, Stage::Review, current.round));
        }
        (Binding::Review, Outcome::Review(ReviewResult::Approved)) => {
            d.state.stage = Some(Stage::Delivery);
            let reason = format!(
                "{key} approved the implementation. Ready for delivery: inspect, validate and approve the \
                 integration; completing the task is your decision."
            );
            finish(d, WorkflowStatus::Completed, reason.clone());
            d.changes.push(Change::Completed(reason));
        }
        (Binding::Review, Outcome::Review(ReviewResult::ChangesRequested)) => {
            let correction = Step::new(Binding::Correct, Stage::Correction, current.round + 1);
            if current.round <= MAX_CORRECTIONS {
                start_or_hold(d, correction);
            } else {
                wait(
                    d,
                    format!(
                        "{key} still requests changes after {MAX_CORRECTIONS} correction rounds. Review the findings; \
                         resume to allow one more correction round, or take over."
                    ),
                    Resume::Start { step: correction },
                );
            }
        }
        (Binding::Review, Outcome::Review(ReviewResult::Rejected)) => {
            wait(
                d,
                format!(
                    "{key} rejected the implementation. Review the findings; resume to start a correction round."
                ),
                Resume::Start { step: Step::new(Binding::Correct, Stage::Correction, current.round + 1) },
            );
        }
        (Binding::Review, Outcome::Review(ReviewResult::Blocked)) => {
            wait(
                d,
                format!(
                    "{key} could not give a verdict (blocked, for example because the workspace changed during the \
                     review). Inspect it; resume to review again."
                ),
                Resume::Start { step: current.retry() },
            );
        }
        (Binding::Review, Outcome::Done) => {
            // A review that reported something other than a verdict.
            wait(
                d,
                format!("{key} ended without a verdict. Resume to review again."),
                Resume::Start { step: current.retry() },
            );
        }
    }
}

/// Starts `step` now, or (while paused) keeps it for the resume. `true` if started.
fn start_or_hold(d: &mut Decision, step: Step) -> bool {
    if d.state.status == WorkflowStatus::Paused {
        d.state.on_resume = Some(Resume::Start { step });
        d.state.reason = Some("paused by the operator; the next step starts when you resume".into());
        return false;
    }
    begin(d, step);
    true
}

fn begin(d: &mut Decision, step: Step) {
    d.state.status = WorkflowStatus::Running;
    d.state.stage = Some(step.stage);
    d.state.current = Some(step.clone());
    d.state.repairs = 0;
    d.state.reason = None;
    d.state.on_resume = None;
    d.changes.push(Change::StageChanged(step.clone()));
    d.action = Action::Start(step);
}

fn run(d: &mut Decision) {
    d.state.status = WorkflowStatus::Running;
    d.state.reason = None;
    d.state.on_resume = None;
}

fn wait(d: &mut Decision, reason: String, on_resume: Resume) {
    d.state.status = WorkflowStatus::Waiting;
    d.state.reason = Some(reason.clone());
    d.state.on_resume = Some(on_resume);
    d.action = Action::None;
    d.changes.push(Change::Waiting(reason));
}

fn finish(d: &mut Decision, status: WorkflowStatus, reason: String) {
    d.state.status = status;
    d.state.reason = Some(reason);
    d.state.on_resume = None;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn apply(state: &WorkflowState, input: Input) -> Decision {
        decide(state, &input).unwrap()
    }

    fn key(d: &Decision) -> String {
        match &d.action {
            Action::Start(s) => s.key(),
            other => panic!("expected a start, got {other:?}"),
        }
    }

    fn report(d: &Decision, outcome: Outcome) -> Decision {
        let step = d.state.current.as_ref().unwrap().key();
        apply(&d.state, Input::Reported { step, outcome })
    }

    #[test]
    fn full_cycle_with_one_correction_reaches_delivery() {
        let d = apply(&WorkflowState::default(), Input::Start);
        assert_eq!(key(&d), "investigate#1");
        assert_eq!(d.changes[0], Change::Started);
        let d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "implement#1");
        let d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "review#1");
        let d = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(key(&d), "implement#2");
        let Action::Start(s) = &d.action else { unreachable!() };
        assert_eq!((s.binding, s.stage, s.corrections()), (Binding::Correct, Stage::Correction, 1));
        let d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "review#2");
        let d = report(&d, Outcome::Review(ReviewResult::Approved));
        assert_eq!(d.state.status, WorkflowStatus::Completed);
        assert_eq!(d.state.stage, Some(Stage::Delivery));
        assert_eq!(d.action, Action::None);
        assert!(matches!(d.changes.last(), Some(Change::Completed(_))));
    }

    #[test]
    fn correction_rounds_are_bounded_then_the_operator_decides() {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d = report(&d, Outcome::Done); // implement#1
        d = report(&d, Outcome::Done); // review#1
        for expected in ["implement#2", "implement#3"] {
            d = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
            assert_eq!(key(&d), expected);
            d = report(&d, Outcome::Done);
        }
        assert_eq!(key(&d), "review#3");
        let waiting = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(waiting.state.status, WorkflowStatus::Waiting);
        assert_eq!(waiting.action, Action::None);
        assert!(waiting.state.reason.as_deref().unwrap().contains("2 correction rounds"));
        // Resuming allows exactly one more round.
        let resumed = apply(&waiting.state, Input::Resume);
        assert_eq!(key(&resumed), "implement#4");
        assert_eq!(resumed.changes[0], Change::Resumed);
    }

    #[test]
    fn rejected_and_blocked_reviews_and_blockers_stop_for_the_operator() {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d = report(&d, Outcome::Done);
        d = report(&d, Outcome::Done);
        let rejected = report(&d, Outcome::Review(ReviewResult::Rejected));
        assert_eq!(rejected.state.status, WorkflowStatus::Waiting);
        assert_eq!(key(&apply(&rejected.state, Input::Resume)), "implement#2");
        let blocked = report(&d, Outcome::Review(ReviewResult::Blocked));
        assert_eq!(blocked.state.status, WorkflowStatus::Waiting);
        assert_eq!(key(&apply(&blocked.state, Input::Resume)), "review#1@2");
        let blocker = report(&d, Outcome::Blocker("no access".into()));
        assert!(blocker.state.reason.as_deref().unwrap().contains("no access"));
    }

    #[test]
    fn unusable_reports_get_two_repairs_then_wait() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let step = "investigate#1".to_owned();
        let u =
            |s: &WorkflowState| apply(s, Input::Unusable { step: step.clone(), error: "no block".into() });
        let r1 = u(&d.state);
        assert_eq!(r1.action, Action::Repair { n: 1 });
        let r2 = u(&r1.state);
        assert_eq!(r2.action, Action::Repair { n: 2 });
        let w = u(&r2.state);
        assert_eq!((w.state.status, &w.action), (WorkflowStatus::Waiting, &Action::None));
        let resumed = apply(&w.state, Input::Resume);
        assert_eq!(resumed.action, Action::Repair { n: 1 });
        assert_eq!(resumed.state.status, WorkflowStatus::Running);
        // A usable report afterwards continues normally and resets the repairs.
        let next = report(&resumed, Outcome::Done);
        assert_eq!((key(&next), next.state.repairs), ("implement#1".to_owned(), 0));
    }

    #[test]
    fn a_failed_step_is_retried_once_with_the_same_binding() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let d = report(&d, Outcome::Done); // implement#1
        let ended = |s: &WorkflowState, step: &str| {
            apply(s, Input::Ended { step: step.into(), cancelled: false, reason: "exit 1".into() })
        };
        let retry = ended(&d.state, "implement#1");
        assert_eq!(key(&retry), "implement#1@2");
        let Action::Start(s) = &retry.action else { unreachable!() };
        assert_eq!(s.binding, Binding::Implement, "same binding: no provider fallback");
        let waiting = ended(&retry.state, "implement#1@2");
        assert_eq!(waiting.state.status, WorkflowStatus::Waiting);
        assert_eq!(key(&apply(&waiting.state, Input::Resume)), "implement#1@3");
        // A cancelled assignment is never retried automatically.
        let cancelled = apply(
            &d.state,
            Input::Ended { step: "implement#1".into(), cancelled: true, reason: "by operator".into() },
        );
        assert_eq!(cancelled.state.status, WorkflowStatus::Waiting);
    }

    #[test]
    fn stale_and_replayed_inputs_change_nothing() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let d = report(&d, Outcome::Done); // now implement#1
        let replay =
            apply(&d.state, Input::Reported { step: "investigate#1".into(), outcome: Outcome::Done });
        assert_eq!((replay.state, replay.action), (d.state.clone(), Action::None));
        assert!(decide(&d.state, &Input::Start).is_err(), "a workflow starts once");
        let done = WorkflowState { status: WorkflowStatus::Completed, ..d.state.clone() };
        let late = apply(&done, Input::Reported { step: "implement#1".into(), outcome: Outcome::Done });
        assert_eq!(late.action, Action::None);
    }

    #[test]
    fn pausing_holds_the_next_step_until_resume() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let paused = apply(&d.state, Input::Pause);
        assert_eq!(paused.state.status, WorkflowStatus::Paused);
        let held = report(&paused, Outcome::Done);
        assert_eq!((held.state.status, &held.action), (WorkflowStatus::Paused, &Action::None));
        assert_eq!(key(&apply(&held.state, Input::Resume)), "implement#1");
    }

    #[test]
    fn questions_interruptions_cancel_and_closed_tasks() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let q =
            apply(&d.state, Input::Question { step: "investigate#1".into(), summary: "which file?".into() });
        assert_eq!(q.state.status, WorkflowStatus::Waiting);
        // The agent answers after the operator's note: the workflow continues by itself.
        assert_eq!(key(&report(&q, Outcome::Done)), "implement#1");
        let i = apply(&d.state, Input::Interrupted { step: "investigate#1".into() });
        assert!(i.state.reason.as_deref().unwrap().contains("Core restarted"));
        // The interrupted session's failure, replayed later, starts nothing.
        let late = apply(
            &i.state,
            Input::Ended { step: "investigate#1".into(), cancelled: false, reason: "x".into() },
        );
        assert_eq!((late.state.clone(), late.action), (i.state.clone(), Action::None));
        assert_eq!(key(&apply(&i.state, Input::Resume)), "investigate#1@2");
        let c = apply(&d.state, Input::Cancel { reason: "not needed".into() });
        assert_eq!((c.state.status, &c.action), (WorkflowStatus::Cancelled, &Action::CancelAssignment));
        let f = apply(&d.state, Input::TaskClosed { status: "failed".into() });
        assert_eq!(f.state.status, WorkflowStatus::Failed);
    }

    #[test]
    fn outcomes_parse_from_assignment_records() {
        assert_eq!(Outcome::from_assignment("reported: findings", None), Some(Outcome::Done));
        assert_eq!(
            Outcome::from_assignment("review: changes_requested", None),
            Some(Outcome::Review(ReviewResult::ChangesRequested))
        );
        assert_eq!(Outcome::from_assignment("blocker", Some("x")), Some(Outcome::Blocker("x".into())));
        assert_eq!(Outcome::from_assignment("session ended", None), None);
    }

    #[test]
    fn state_round_trips_through_json() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let w = apply(&d.state, Input::Interrupted { step: "investigate#1".into() });
        let json = serde_json::to_string(&w.state).unwrap();
        let back: WorkflowState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w.state);
    }
}
