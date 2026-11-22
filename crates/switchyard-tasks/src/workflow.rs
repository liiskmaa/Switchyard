//! The built-in engineering workflow (`engineering@1`, milestone 8.1, ADR 0033;
//! `engineering@2` adds review triage, ADR 0038), as a pure decision function.
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
//! `implement#1@2`.
//!
//! Review triage (ADR 0038): in workflows started with it, a review that
//! requests changes goes to reasoning first (`investigate#n+1`, stage
//! `triage`, the investigation binding). Reasoning decides `fix` (the
//! correction `implement#n+1` follows) or `dispute` (the reviewer gets the
//! reasoning and reviews again as `review#n+1`, skipping implementation).
//! Fixes and disputes share the [`MAX_CORRECTIONS`] rounds; then a person
//! breaks the tie.
//!
//! Test gate (ADR 0039): in workflows started with it, a finished
//! implementation goes to stage `testing`: Core runs the test commands the
//! implementer named (unit, integration, regression; browser when the UI
//! changed) and reports [`Input::Tested`]. Passing tests start the review
//! with their results as evidence; failing ones go to reasoning like review
//! findings, sharing the same rounds.
//!
//! Subtasks (ADR 0040): when the first investigation reports a plan of 2 to
//! [`MAX_SUBTASKS`] subtasks, the workflow takes it as its work plan and runs
//! each subtask through implementation, tests, review and reasoning in turn
//! (step keys `s2/implement#1`, …). A subtask is done when its review
//! approves; after the last one the task goes to delivery once.
//!
//! There is no separate correction role (ADR 0038): a fix is another
//! implementation round (`implement#n`, stage `implementation`) with the
//! implementation binding. `Binding::Correct` and `Stage::Correction` remain
//! only so states stored by earlier versions still load and resume. Core stores the key with a unique index, so replaying an
//! event can never create the same step twice.
//!
//! Nothing here performs I/O or knows about providers.

use serde::{Deserialize, Serialize};

use crate::model::{ReviewResult, Role};
use crate::report::TriageDecision;

pub const DEFINITION: &str = "engineering";
/// v2 (ADR 0038): review findings go to reasoning before implementation.
pub const VERSION: u32 = 2;
/// Automatic correction rounds after a review requests changes.
pub const MAX_CORRECTIONS: u32 = 2;
/// Automatic repair notes for a turn without a usable report.
pub const MAX_REPAIRS: u32 = 2;
/// Automatic retries of a step whose session failed.
pub const MAX_RETRIES: u32 = 1;
/// Most subtasks reasoning's plan may split a task into (ADR 0040).
pub const MAX_SUBTASKS: usize = 8;

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
    /// Reasoning decides whether a review's findings should be fixed (ADR 0038).
    Triage,
    /// Core runs the implementer's test commands before the review (ADR 0039).
    Testing,
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
            Self::Triage => "triage",
            Self::Testing => "testing",
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
    /// M8.1 runs only: the review approved and the workflow stopped at the
    /// delivery gate (M8.2 workflows continue into delivery instead).
    Completed,
    /// The approved integration landed on the target branch (M8.2).
    Delivered,
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
            Self::Delivered => "delivered",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn is_final(self) -> bool {
        matches!(self, Self::Completed | Self::Delivered | Self::Cancelled | Self::Failed)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "waiting" => Self::Waiting,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "delivered" => Self::Delivered,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// Whether `step_key` names a review-triage step: an investigation after the
/// first (`investigate#2`, `investigate#3@2`, …; ADR 0038).
/// A subtask never starts with an investigation, so any investigation inside
/// one (`s2/investigate#2`) is a triage too.
pub fn is_triage_step(step_key: &str) -> bool {
    let (subtask, key) = split_subtask(step_key);
    let round = key
        .strip_prefix("investigate#")
        .and_then(|rest| rest.split('@').next())
        .and_then(|n| n.parse::<u32>().ok());
    round.is_some_and(|n| n > 1 || subtask.is_some())
}

/// The subtask a step key belongs to and the key without it:
/// `s2/implement#1` → `(Some(2), "implement#1")`.
pub fn split_subtask(step_key: &str) -> (Option<u32>, &str) {
    if let Some(rest) = step_key.strip_prefix('s')
        && let Some((n, key)) = rest.split_once('/')
        && let Ok(n) = n.parse::<u32>()
    {
        return (Some(n), key);
    }
    (None, step_key)
}

/// One subtask of reasoning's plan (ADR 0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedSubtask {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// Its review approved.
    #[serde(default)]
    pub done: bool,
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
    /// The plan's subtask this step works on, 1-based (ADR 0040).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtask: Option<u32>,
}

impl Step {
    fn new(binding: Binding, stage: Stage, round: u32) -> Self {
        Self { binding, stage, round, attempt: 1, subtask: None }
    }

    /// A step of the same subtask as `current`.
    fn within(current: &Step, binding: Binding, stage: Stage, round: u32) -> Self {
        Self { subtask: current.subtask, ..Self::new(binding, stage, round) }
    }

    /// The unique step key, e.g. `implement#2`, `review#1@2` or `s2/implement#1`.
    pub fn key(&self) -> String {
        let base = format!("{}#{}", self.binding.role().as_str(), self.round);
        let base = if self.attempt > 1 { format!("{base}@{}", self.attempt) } else { base };
        match self.subtask {
            Some(n) => format!("s{n}/{base}"),
            None => base,
        }
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
    /// The current assignment is still open (a paused step still running,
    /// or an agent's question in a workflow stored before `Answer`): just
    /// continue.
    Continue,
    /// The current step's agent asked the operator this question. Only the
    /// operator's answer, delivered to the agent as a note
    /// (`Input::Answered`), continues; resuming without one is refused, so
    /// the workflow never shows "running" while its agent waits.
    Answer { question: String },
    /// Send the current assignment another repair note.
    Repair,
    /// Start this step.
    Start { step: Step },
    /// Check the delivery again: prepare (or re-prepare) the integration.
    Deliver,
    /// Run the validation again.
    Validate,
    /// Run the test gate again; `authorized` means the operator's resume
    /// authorizes the commands the policy asks about (ADR 0039).
    RunTests { authorized: bool },
}

/// The durable state of one workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// Delivery (M8.2): the last delivery phase seen (see [`Delivery::phase`]).
    pub delivery: Option<String>,
    /// Delivery: the source commit the workflow already asked to validate
    /// (validation is started at most once per commit automatically).
    pub validated_source: Option<String>,
    /// Review findings go to reasoning before implementation (ADR 0038).
    /// Set when the workflow starts; workflows started before it keep the
    /// direct review → correction routing.
    pub review_triage: bool,
    /// A finished implementation is tested by Core before the review
    /// (ADR 0039). Set when the workflow starts.
    pub test_gate: bool,
    /// Reasoning's plan, when it split the task (ADR 0040).
    pub plan: Vec<PlannedSubtask>,
}

/// Where the delivery of the reviewed work stands, read by Core from the
/// task's M7 integration record (its status, blockers, validation runs and
/// the policy's decision for each validation command). Nothing here comes
/// from an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The reviewed changes are not committed; only the operator commits (ADR 0029).
    NeedsCommit,
    /// Validation commands are configured, all allowed by policy, and no run
    /// exists for this source commit.
    NeedsValidation {
        source: String,
    },
    /// Some validation commands need the operator's authorization (policy `ask`).
    NeedsAuthorization {
        commands: usize,
    },
    Validating,
    ValidationFailed {
        reason: String,
    },
    /// Something the operator must resolve (conflicts, the target moved or is
    /// dirty, a denied command, …).
    Blocked {
        reason: String,
    },
    /// Ready: waiting for the operator's approval of the exact commits.
    AwaitingApproval {
        validated: bool,
    },
    Integrating,
    Integrated {
        merge_commit: String,
        target_branch: String,
    },
    Failed {
        reason: String,
    },
    Cancelled,
}

impl Delivery {
    pub fn phase(&self) -> &'static str {
        match self {
            Self::NeedsCommit => "needs_commit",
            Self::NeedsValidation { .. } => "validation_starting",
            Self::NeedsAuthorization { .. } => "awaiting_validation_authorization",
            Self::Validating => "validating",
            Self::ValidationFailed { .. } => "validation_failed",
            Self::Blocked { .. } => "blocked",
            Self::AwaitingApproval { .. } => "awaiting_approval",
            Self::Integrating => "integrating",
            Self::Integrated { .. } => "delivered",
            Self::Failed { .. } => "delivery_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// How a step's assignment ended with a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Findings, a proposal or an implementation report: the step is done.
    Done,
    Review(ReviewResult),
    /// The agent cannot continue.
    Blocker(String),
    /// Reasoning's decision on a review's findings (ADR 0038).
    Triage(TriageDecision),
    /// The first investigation planned these subtasks (ADR 0040).
    Plan(Vec<PlannedSubtask>),
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
        match outcome {
            "triage: fix" => return Some(Self::Triage(TriageDecision::Fix)),
            "triage: dispute" => return Some(Self::Triage(TriageDecision::Dispute)),
            _ => {}
        }
        if outcome == "blocker" {
            return Some(Self::Blocker(summary.unwrap_or("no reason given").to_owned()));
        }
        outcome.starts_with("reported: ").then_some(Self::Done)
    }
}

/// The result of the test gate for an implementation step (ADR 0039).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tested {
    /// Every required kind ran and every command passed.
    Passed(String),
    /// A command failed, was blocked, or a required kind is missing.
    Failed(String),
    /// Some commands need the operator's authorization (policy `ask`).
    NeedsAuthorization(String),
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
    /// The step's agent asked the operator a question; the assignment stays
    /// open. `summary` is the question as the agent put it.
    Question {
        step: String,
        summary: String,
    },
    /// The operator's note answering the step's question was delivered to
    /// its agent (the agent is working again).
    Answered {
        step: String,
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
    /// Delivery (M8.2): the integration's current state.
    Delivery(Delivery),
    /// Delivery (M8.2): the operator sends the work back for a correction
    /// round (for example after a failed validation).
    RequestCorrection,
    /// The test gate finished for this implementation step (ADR 0039).
    Tested {
        step: String,
        result: Tested,
    },
    /// Core restarted while the test gate ran.
    TestsInterrupted,
    /// The operator retried the task after it failed: a workflow that ended
    /// with it comes back and waits to continue where it stopped.
    TaskReopened,
    /// The operator (through the task's supervisor) sends the work to
    /// `target` now: a new step of that role (a new round), or the test gate
    /// for the current implementation. Only while no agent of the workflow
    /// is working; Core delivers an instruction for the working agent itself.
    Directed {
        target: Target,
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
    /// Delivery: create the task's integration, or prepare its existing one again.
    Prepare,
    /// Delivery: run the project's validation (only commands the policy allows).
    Validate,
    /// Delivery: the integration landed; record the task as completed.
    CompleteTask,
    /// Run the test gate for the current implementation step (ADR 0039).
    RunTests {
        authorized: bool,
    },
}

/// Where the operator (through the task's supervisor) sends work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Reasoning,
    Coding,
    Review,
    Tests,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Coding => "coding",
            Self::Review => "review",
            Self::Tests => "tests",
        }
    }

    /// The role whose agent does this work (`None` for the tests: Core runs them).
    pub fn binding(self) -> Option<Binding> {
        match self {
            Self::Reasoning => Some(Binding::Investigate),
            Self::Coding => Some(Binding::Implement),
            Self::Review => Some(Binding::Review),
            Self::Tests => None,
        }
    }
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
    /// Delivery progressed: phase and explanation.
    Delivery(&'static str, String),
    /// The test gate: `running`, `passed`, `failed` or `needs_authorization`, and why.
    Tests(&'static str, String),
    /// Reasoning's plan was taken (ADR 0040): the subtasks' titles.
    Planned(Vec<String>),
    /// A subtask's review approved: its number and title.
    SubtaskDone(u32, String),
    /// The operator sent the work to this target (through the supervisor).
    Directed(Target),
    Delivered(String),
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
    #[error(
        "the agent asked a question and is waiting for your answer: answer it with a note on the task page"
    )]
    NeedsAnswer,
    /// Work cannot be redirected now; the reason, in a sentence.
    #[error("{0}")]
    Busy(String),
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
            d.state.review_triage = true;
            d.state.test_gate = true;
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
            // The operator continues a workflow that ended with its task
            // (ADR 0040 addendum): the step that stopped runs again.
            WorkflowStatus::Failed if state.current.is_some() => {
                let step = state
                    .current
                    .clone()
                    .unwrap_or_else(|| Step::new(Binding::Investigate, Stage::Investigation, 1));
                d.changes.push(Change::Resumed);
                match state.stage {
                    Some(Stage::Delivery) => {
                        run(&mut d);
                        d.action = Action::Prepare;
                    }
                    Some(Stage::Testing) => {
                        run(&mut d);
                        d.action = Action::RunTests { authorized: false };
                    }
                    _ => begin(&mut d, step.retry()),
                }
            }
            WorkflowStatus::Waiting | WorkflowStatus::Paused => {
                d.changes.push(Change::Resumed);
                match state.on_resume.clone().unwrap_or(Resume::Continue) {
                    Resume::Answer { .. } => return Err(WorkflowError::NeedsAnswer),
                    Resume::Continue => run(&mut d),
                    Resume::Repair => {
                        run(&mut d);
                        d.state.repairs = 1;
                        d.action = Action::Repair { n: 1 };
                    }
                    Resume::Start { step } => begin(&mut d, step),
                    Resume::Deliver => {
                        run(&mut d);
                        d.action = Action::Prepare;
                    }
                    Resume::Validate => {
                        run(&mut d);
                        d.state.validated_source = None;
                        d.action = Action::Validate;
                    }
                    Resume::RunTests { authorized } => {
                        run(&mut d);
                        d.state.stage = Some(Stage::Testing);
                        d.changes.push(Change::Tests("running", "Running the tests again.".into()));
                        d.action = Action::RunTests { authorized };
                    }
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
            let reason = format!("the task was {task_status}");
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
        Input::Delivery(fact) => {
            if status.is_final() || state.stage != Some(Stage::Delivery) {
                return Ok(d);
            }
            delivery(&mut d, fact);
        }
        Input::RequestCorrection => {
            if status.is_final() || state.stage != Some(Stage::Delivery) {
                return Err(WorkflowError::NotAllowed("not delivering"));
            }
            if matches!(state.delivery.as_deref(), Some("integrating" | "validating")) {
                return Err(WorkflowError::NotAllowed("busy delivering (validating or integrating)"));
            }
            let round = state.current.as_ref().map(|s| s.round).unwrap_or(1);
            d.state.validated_source = None;
            d.state.delivery = None;
            begin(&mut d, Step::new(Binding::Implement, Stage::Implementation, round + 1));
        }
        Input::Tested { step, result } => {
            if state.stage != Some(Stage::Testing) {
                return Ok(d);
            }
            let Some(current) = current_for(state, step) else { return Ok(d) };
            tested(&mut d, &current, result);
        }
        Input::TaskReopened => {
            if status != WorkflowStatus::Failed {
                return Ok(d);
            }
            let Some(step) = state.current.clone() else { return Ok(d) };
            d.changes.push(Change::Resumed);
            if state.stage == Some(Stage::Delivery) {
                wait(
                    &mut d,
                    "The task was retried. Resume to check the delivery again.".into(),
                    Resume::Deliver,
                );
            } else if state.stage == Some(Stage::Testing) {
                wait(
                    &mut d,
                    "The task was retried. Resume to run the tests again.".into(),
                    Resume::RunTests { authorized: false },
                );
            } else {
                let key = step.key();
                wait(
                    &mut d,
                    format!("The task was retried. Resume to run {key} again."),
                    Resume::Start { step: step.retry() },
                );
            }
        }
        Input::TestsInterrupted => {
            if state.stage == Some(Stage::Testing) && state.status == WorkflowStatus::Running {
                wait(
                    &mut d,
                    "Core restarted while the tests ran. Resume to run them again.".into(),
                    Resume::RunTests { authorized: false },
                );
            }
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
            let q = summary.trim();
            let stop = if q.ends_with(['.', '?', '!']) { "" } else { "." };
            wait(
                &mut d,
                format!(
                    "{step} asks you: {q}{stop} Answer it; the workflow continues when the agent reports."
                ),
                Resume::Answer { question: q.to_owned() },
            );
        }
        Input::Directed { target } => directed(&mut d, state, *target)?,
        Input::Answered { step } => {
            let asked = matches!(state.on_resume, Some(Resume::Answer { .. } | Resume::Continue));
            if status != WorkflowStatus::Waiting || !asked || current_for(state, step).is_none() {
                return Ok(d);
            }
            d.changes.push(Change::Resumed);
            run(&mut d);
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
        (Binding::Investigate, outcome) if current.stage == Stage::Triage => {
            match outcome {
                Outcome::Triage(TriageDecision::Dispute) => {
                    // Skip implementation: the reviewer gets the reasoning.
                    start_or_hold(d, Step::within(current, Binding::Review, Stage::Review, current.round));
                }
                // `fix`, or a report without a decision: the findings are addressed.
                _ => {
                    start_or_hold(
                        d,
                        Step::within(current, Binding::Implement, Stage::Implementation, current.round),
                    );
                }
            }
        }
        (Binding::Investigate, Outcome::Plan(items)) if current.subtask.is_none() && items.len() >= 2 => {
            let first = Step { subtask: Some(1), ..Step::new(Binding::Implement, Stage::Implementation, 1) };
            if items.len() > MAX_SUBTASKS {
                wait(
                    d,
                    format!(
                        "{key} planned {} subtasks, more than the limit of {MAX_SUBTASKS}. Send a note asking for a \
                         smaller plan and resume to implement the task as one piece, or take over.",
                        items.len()
                    ),
                    Resume::Start { step: Step::new(Binding::Implement, Stage::Implementation, 1) },
                );
                return;
            }
            d.state.plan = items.iter().map(|s| PlannedSubtask { done: false, ..s.clone() }).collect();
            d.changes.push(Change::Planned(items.iter().map(|s| s.title.clone()).collect()));
            start_or_hold(d, first);
        }
        (Binding::Investigate, _) => {
            start_or_hold(d, Step::within(current, Binding::Implement, Stage::Implementation, 1));
        }
        (Binding::Implement | Binding::Correct, _) if d.state.test_gate => {
            // ADR 0039: Core tests the work before anyone reviews it.
            d.state.stage = Some(Stage::Testing);
            let reason = format!("Running the tests {key} named before the review.");
            d.changes.push(Change::Tests("running", reason.clone()));
            if d.state.status == WorkflowStatus::Paused {
                d.state.on_resume = Some(Resume::RunTests { authorized: false });
                d.state.reason = Some("paused by the operator; the tests run when you resume".into());
            } else {
                run(d);
                d.state.reason = Some(reason);
                d.action = Action::RunTests { authorized: false };
            }
        }
        (Binding::Implement | Binding::Correct, _) => {
            start_or_hold(d, Step::within(current, Binding::Review, Stage::Review, current.round));
        }
        (Binding::Review, Outcome::Review(ReviewResult::Approved))
            if current.subtask.is_some_and(|n| (n as usize) < d.state.plan.len()) =>
        {
            // ADR 0040: this subtask is done; the next one starts.
            let n = current.subtask.unwrap_or(1);
            if let Some(s) = d.state.plan.get_mut(n as usize - 1) {
                s.done = true;
                d.changes.push(Change::SubtaskDone(n, s.title.clone()));
            }
            let next =
                Step { subtask: Some(n + 1), ..Step::new(Binding::Implement, Stage::Implementation, 1) };
            start_or_hold(d, next);
        }
        (Binding::Review, Outcome::Review(ReviewResult::Approved)) => {
            // ADR 0040: the last subtask's approval completes the plan.
            if let Some(n) = current.subtask
                && let Some(s) = d.state.plan.get_mut(n as usize - 1)
            {
                s.done = true;
                d.changes.push(Change::SubtaskDone(n, s.title.clone()));
            }
            // M8.2: the approved work goes on to delivery (prepare, validate,
            // the operator's approval, integration).
            d.state.stage = Some(Stage::Delivery);
            d.state.validated_source = None;
            d.state.delivery = Some("preparing".into());
            let reason = format!("{key} approved the implementation; preparing its integration.");
            d.changes.push(Change::Delivery("preparing", reason.clone()));
            if d.state.status == WorkflowStatus::Paused {
                d.state.on_resume = Some(Resume::Deliver);
                d.state.reason = Some("paused by the operator; delivery continues when you resume".into());
            } else {
                run(d);
                d.state.reason = Some(reason);
                d.action = Action::Prepare;
            }
        }
        (Binding::Review, Outcome::Review(ReviewResult::ChangesRequested)) => {
            let next = if d.state.review_triage {
                Step::within(current, Binding::Investigate, Stage::Triage, current.round + 1)
            } else {
                Step::within(current, Binding::Implement, Stage::Implementation, current.round + 1)
            };
            if current.round <= MAX_CORRECTIONS {
                start_or_hold(d, next);
            } else if d.state.review_triage {
                wait(
                    d,
                    format!(
                        "{key} still requests changes after {MAX_CORRECTIONS} rounds of fixes or reasoning. Read the \
                         findings and the reasoning; resume to let reasoning decide once more, send a note, or take \
                         over."
                    ),
                    Resume::Start { step: next },
                );
            } else {
                wait(
                    d,
                    format!(
                        "{key} still requests changes after {MAX_CORRECTIONS} correction rounds. Review the findings; \
                         resume to allow one more correction round, or take over."
                    ),
                    Resume::Start { step: next },
                );
            }
        }
        (Binding::Review, Outcome::Review(ReviewResult::Rejected)) => {
            wait(
                d,
                format!(
                    "{key} rejected the implementation. Review the findings; resume to start a correction round."
                ),
                Resume::Start {
                    step: Step::within(current, Binding::Implement, Stage::Implementation, current.round + 1),
                },
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
        (Binding::Review, Outcome::Done | Outcome::Triage(_) | Outcome::Plan(_)) => {
            // A review that reported something other than a verdict.
            wait(
                d,
                format!("{key} ended without a verdict. Resume to review again."),
                Resume::Start { step: current.retry() },
            );
        }
    }
}

/// Whether an agent of the workflow works (or waits, with its assignment
/// open) on the current step.
pub fn agent_busy(state: &WorkflowState) -> bool {
    match state.status {
        WorkflowStatus::Running => !matches!(state.stage, Some(Stage::Testing | Stage::Delivery)),
        WorkflowStatus::Waiting | WorkflowStatus::Paused => {
            matches!(state.on_resume, None | Some(Resume::Continue | Resume::Answer { .. } | Resume::Repair))
        }
        _ => false,
    }
}

/// `Input::Directed`: a new step for `target`, in a new round of the current
/// subtask, or the test gate for the current implementation.
fn directed(d: &mut Decision, state: &WorkflowState, target: Target) -> Result<(), WorkflowError> {
    let status = state.status;
    if status.is_final() || status == WorkflowStatus::Pending {
        return Err(WorkflowError::NotAllowed(status.as_str()));
    }
    let Some(current) = state.current.clone() else {
        return Err(WorkflowError::NotAllowed("not started"));
    };
    if agent_busy(state) {
        return Err(WorkflowError::Busy(format!(
            "{} is still open; answer or guide its agent, or wait until it reports",
            current.key()
        )));
    }
    if state.stage == Some(Stage::Testing) && status == WorkflowStatus::Running {
        return Err(WorkflowError::Busy("Switchyard is running the tests".into()));
    }
    if state.stage == Some(Stage::Delivery)
        && matches!(state.delivery.as_deref(), Some("integrating" | "validating"))
    {
        return Err(WorkflowError::Busy("the delivery is validating or integrating".into()));
    }
    d.changes.push(Change::Directed(target));
    d.state.validated_source = None;
    d.state.delivery = None;
    let round = current.round + 1;
    match target {
        Target::Tests => {
            if current.binding != Binding::Implement || !state.test_gate {
                return Err(WorkflowError::Busy(
                    "there is no finished implementation to test right now; send the work to coding or review".into(),
                ));
            }
            run(d);
            d.state.stage = Some(Stage::Testing);
            d.changes.push(Change::Tests("running", "Running the tests again, as you asked.".into()));
            d.action = Action::RunTests { authorized: false };
        }
        Target::Reasoning => begin(d, Step::within(&current, Binding::Investigate, Stage::Triage, round)),
        Target::Coding => begin(d, Step::within(&current, Binding::Implement, Stage::Implementation, round)),
        Target::Review => begin(d, Step::within(&current, Binding::Review, Stage::Review, round)),
    }
    Ok(())
}

/// The test gate's result for `current` (an implementation step, ADR 0039).
fn tested(d: &mut Decision, current: &Step, result: &Tested) {
    let key = current.key();
    match result {
        Tested::Passed(summary) => {
            d.changes.push(Change::Tests("passed", summary.clone()));
            start_or_hold(d, Step::within(current, Binding::Review, Stage::Review, current.round));
        }
        Tested::Failed(why) => {
            d.changes.push(Change::Tests("failed", why.clone()));
            let next = Step::within(current, Binding::Investigate, Stage::Triage, current.round + 1);
            if current.round <= MAX_CORRECTIONS {
                start_or_hold(d, next);
            } else {
                wait(
                    d,
                    format!(
                        "The tests of {key} still fail after {MAX_CORRECTIONS} rounds ({why}). Read the results; resume \
                         to let reasoning decide once more, send a note, or take over."
                    ),
                    Resume::Start { step: next },
                );
            }
        }
        Tested::NeedsAuthorization(commands) => {
            d.changes.push(Change::Tests("needs_authorization", commands.clone()));
            wait(
                d,
                format!(
                    "The tests of {key} include commands your policies ask about: {commands}. Resume to authorize and \
                     run them, or turn on auto mode for the project."
                ),
                Resume::RunTests { authorized: true },
            );
        }
    }
}

/// The delivery stage: follows the integration's state (M8.2).
fn delivery(d: &mut Decision, fact: &Delivery) {
    let phase = fact.phase();
    let unchanged = d.state.delivery.as_deref() == Some(phase);
    d.state.delivery = Some(phase.into());
    match fact {
        Delivery::NeedsValidation { source } => {
            if d.state.validated_source.as_deref() == Some(source.as_str()) {
                return; // already asked for this commit; its run decides
            }
            if d.state.status == WorkflowStatus::Paused {
                d.state.on_resume = Some(Resume::Validate);
                return;
            }
            d.state.validated_source = Some(source.clone());
            run(d);
            let reason = "Validating the committed changes with the project's commands.".to_owned();
            d.state.reason = Some(reason.clone());
            d.changes.push(Change::Delivery(phase, reason));
            d.action = Action::Validate;
        }
        Delivery::Validating | Delivery::Integrating => {
            if unchanged && d.state.status == WorkflowStatus::Running {
                return;
            }
            if d.state.status != WorkflowStatus::Paused {
                run(d);
            }
            let reason = if matches!(fact, Delivery::Validating) {
                "Validation is running."
            } else {
                "Integrating the approved commits into the target branch."
            };
            d.state.reason = Some(reason.into());
            d.changes.push(Change::Delivery(phase, reason.into()));
        }
        Delivery::Integrated { merge_commit, target_branch } => {
            let reason = format!("Delivered: {target_branch} is at merge commit {merge_commit}.");
            finish(d, WorkflowStatus::Delivered, reason.clone());
            d.changes.push(Change::Delivered(reason));
            d.action = Action::CompleteTask;
        }
        other => {
            let (reason, resume) = match other {
                Delivery::NeedsCommit => (
                    "The reviewed changes are not committed yet. Commit them to validate and deliver them \
                     (Switchyard never commits on its own)."
                        .to_owned(),
                    Resume::Deliver,
                ),
                Delivery::NeedsAuthorization { commands } => (
                    format!(
                        "{commands} validation command(s) need your authorization before they run (project \
                         policy: ask)."
                    ),
                    Resume::Deliver,
                ),
                Delivery::ValidationFailed { reason } => (
                    format!(
                        "Validation failed: {reason}. Send the work back for a correction, or run validation \
                         again."
                    ),
                    Resume::Validate,
                ),
                Delivery::Blocked { reason } => (format!("Delivery is blocked: {reason}"), Resume::Deliver),
                Delivery::AwaitingApproval { validated } => (
                    if *validated {
                        "Validated and ready: review the exact commits and approve the integration."
                            .to_owned()
                    } else {
                        "Ready: no validation commands are configured for this project, so nothing was \
                         validated. Review the exact commits and approve the integration if you accept that."
                            .to_owned()
                    },
                    Resume::Deliver,
                ),
                Delivery::Failed { reason } => (format!("The integration failed: {reason}"), Resume::Deliver),
                Delivery::Cancelled => (
                    "The integration was cancelled. Resume to prepare a new one.".to_owned(),
                    Resume::Deliver,
                ),
                Delivery::NeedsValidation { .. }
                | Delivery::Validating
                | Delivery::Integrating
                | Delivery::Integrated { .. } => return,
            };
            if unchanged
                && d.state.status == WorkflowStatus::Waiting
                && d.state.reason.as_deref() == Some(&reason)
            {
                return;
            }
            wait(d, reason, resume);
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

    /// Reports `outcome` for the current step; a finished implementation's
    /// test gate passes (tested separately below).
    fn report(d: &Decision, outcome: Outcome) -> Decision {
        let step = d.state.current.as_ref().unwrap().key();
        let next = apply(&d.state, Input::Reported { step: step.clone(), outcome });
        if matches!(next.action, Action::RunTests { .. }) {
            return apply(&next.state, Input::Tested { step, result: Tested::Passed("all passed".into()) });
        }
        next
    }

    fn reported_raw(d: &Decision, outcome: Outcome) -> Decision {
        let step = d.state.current.as_ref().unwrap().key();
        apply(&d.state, Input::Reported { step, outcome })
    }

    fn plan(n: usize) -> Outcome {
        Outcome::Plan(
            (1..=n)
                .map(|i| PlannedSubtask {
                    title: format!("part {i}"),
                    description: None,
                    acceptance_criteria: vec![],
                    done: false,
                })
                .collect(),
        )
    }

    #[test]
    fn a_plan_runs_each_subtask_through_the_loop_then_delivers_once() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let d = report(&d, plan(2));
        assert_eq!(key(&d), "s1/implement#1");
        assert!(d.changes.contains(&Change::Planned(vec!["part 1".into(), "part 2".into()])));
        let d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "s1/review#1");
        // A finding inside a subtask: reasoning and the fix stay in that subtask.
        let d = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(key(&d), "s1/investigate#2");
        assert!(is_triage_step("s1/investigate#2") && !is_triage_step("s1/implement#2"));
        assert_eq!(split_subtask("s1/investigate#2"), (Some(1), "investigate#2"));
        let d = report(&d, Outcome::Triage(TriageDecision::Fix));
        assert_eq!(key(&d), "s1/implement#2");
        let d = report(&d, Outcome::Done);
        let d = report(&d, Outcome::Review(ReviewResult::Approved));
        // Subtask 1 is done; subtask 2 starts with its own rounds.
        assert_eq!(key(&d), "s2/implement#1");
        assert!(d.state.plan[0].done && !d.state.plan[1].done);
        assert!(d.changes.contains(&Change::SubtaskDone(1, "part 1".into())));
        let d = report(&d, Outcome::Done);
        let d = report(&d, Outcome::Review(ReviewResult::Approved));
        assert_eq!((d.state.stage, d.action.clone()), (Some(Stage::Delivery), Action::Prepare));
        assert!(d.state.plan.iter().all(|s| s.done));
    }

    #[test]
    fn small_or_oversized_plans() {
        let d = apply(&WorkflowState::default(), Input::Start);
        // One subtask is just the task.
        assert_eq!(key(&report(&d, plan(1))), "implement#1");
        // Too many: a person decides.
        let w = report(&d, plan(MAX_SUBTASKS + 1));
        assert_eq!(w.state.status, WorkflowStatus::Waiting);
        assert!(w.state.plan.is_empty());
        assert_eq!(key(&apply(&w.state, Input::Resume)), "implement#1");
    }

    #[test]
    fn a_retried_task_brings_its_failed_workflow_back() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let d = report(&d, Outcome::Done); // implement#1 running
        let failed = apply(&d.state, Input::TaskClosed { status: "failed".into() });
        assert_eq!(failed.state.status, WorkflowStatus::Failed);
        let back = apply(&failed.state, Input::TaskReopened);
        assert_eq!(back.state.status, WorkflowStatus::Waiting);
        assert!(back.state.reason.as_deref().unwrap().contains("implement#1"));
        assert_eq!(key(&apply(&back.state, Input::Resume)), "implement#1@2");
        // Or the operator continues the failed workflow directly.
        assert_eq!(key(&apply(&failed.state, Input::Resume)), "implement#1@2");
        // Cancelled or delivered workflows stay ended; a running one is unaffected.
        let cancelled = apply(&d.state, Input::Cancel { reason: "x".into() });
        assert_eq!(apply(&cancelled.state, Input::TaskReopened).state, cancelled.state);
        assert_eq!(apply(&d.state, Input::TaskReopened).state, d.state);
    }

    #[test]
    fn the_test_gate_runs_before_every_review() {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        assert!(d.state.test_gate);
        d = report(&d, Outcome::Done); // implement#1
        let t = reported_raw(&d, Outcome::Done);
        assert_eq!(t.action, Action::RunTests { authorized: false });
        assert_eq!((t.state.stage, t.state.status), (Some(Stage::Testing), WorkflowStatus::Running));
        // Stale or foreign results change nothing.
        let other =
            apply(&t.state, Input::Tested { step: "implement#9".into(), result: Tested::Passed("x".into()) });
        assert_eq!(other.action, Action::None);
        // Failing tests go to reasoning, not to review.
        let failed = apply(
            &t.state,
            Input::Tested { step: "implement#1".into(), result: Tested::Failed("unit failed".into()) },
        );
        assert_eq!(key(&failed), "investigate#2");
        assert!(failed.changes.contains(&Change::Tests("failed", "unit failed".into())));
        // Passing tests start the review.
        let passed = apply(
            &t.state,
            Input::Tested { step: "implement#1".into(), result: Tested::Passed("3 passed".into()) },
        );
        assert_eq!(key(&passed), "review#1");
        // Commands the policy asks about wait; resuming authorizes them.
        let ask = apply(
            &t.state,
            Input::Tested {
                step: "implement#1".into(),
                result: Tested::NeedsAuthorization("npm test".into()),
            },
        );
        assert_eq!(ask.state.status, WorkflowStatus::Waiting);
        assert!(ask.state.reason.as_deref().unwrap().contains("npm test"));
        let resumed = apply(&ask.state, Input::Resume);
        assert_eq!(resumed.action, Action::RunTests { authorized: true });
        // A restart while testing waits; resuming runs them again, unauthorized.
        let restarted = apply(&t.state, Input::TestsInterrupted);
        assert_eq!(restarted.state.status, WorkflowStatus::Waiting);
        assert_eq!(apply(&restarted.state, Input::Resume).action, Action::RunTests { authorized: false });
        // Paused: the tests wait for the resume.
        let paused = apply(&d.state, Input::Pause);
        let held = reported_raw(&paused, Outcome::Done);
        assert_eq!((held.action.clone(), held.state.status), (Action::None, WorkflowStatus::Paused));
        assert_eq!(apply(&held.state, Input::Resume).action, Action::RunTests { authorized: false });
        // Workflows started before the gate review directly.
        let mut old = apply(&WorkflowState::default(), Input::Start);
        old.state.test_gate = false;
        old = reported_raw(&old, Outcome::Done);
        assert_eq!(key(&reported_raw(&old, Outcome::Done)), "review#1");
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
        // Review triage (ADR 0038): reasoning decides first, with the investigation binding.
        assert_eq!(key(&d), "investigate#2");
        let Action::Start(s) = &d.action else { unreachable!() };
        assert_eq!((s.binding, s.stage), (Binding::Investigate, Stage::Triage));
        assert!(is_triage_step(&s.key()) && !is_triage_step("investigate#1"));
        let d = report(&d, Outcome::Triage(TriageDecision::Fix));
        assert_eq!(key(&d), "implement#2");
        let Action::Start(s) = &d.action else { unreachable!() };
        assert_eq!((s.binding, s.stage, s.corrections()), (Binding::Implement, Stage::Implementation, 1));
        let d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "review#2");
        let d = report(&d, Outcome::Review(ReviewResult::Approved));
        // M8.2: an approved review starts the delivery; it does not finish the workflow.
        assert_eq!((d.state.status, d.state.stage), (WorkflowStatus::Running, Some(Stage::Delivery)));
        assert_eq!(d.action, Action::Prepare);
    }

    fn delivering() -> WorkflowState {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d = report(&d, Outcome::Done);
        d = report(&d, Outcome::Done);
        report(&d, Outcome::Review(ReviewResult::Approved)).state
    }

    fn deliver(s: &WorkflowState, fact: Delivery) -> Decision {
        apply(s, Input::Delivery(fact))
    }

    #[test]
    fn delivery_runs_validation_once_per_commit_and_waits_for_the_operator() {
        let s = delivering();
        // Uncommitted reviewed work: the operator commits (Switchyard never does).
        let c = deliver(&s, Delivery::NeedsCommit);
        assert_eq!((c.state.status, &c.action), (WorkflowStatus::Waiting, &Action::None));
        assert!(c.state.reason.as_deref().unwrap().contains("never commits"));
        // Committed, all commands allowed: validation starts automatically, once.
        let v = deliver(&c.state, Delivery::NeedsValidation { source: "abc".into() });
        assert_eq!((v.state.status, &v.action), (WorkflowStatus::Running, &Action::Validate));
        let again = deliver(&v.state, Delivery::NeedsValidation { source: "abc".into() });
        assert_eq!(again.action, Action::None, "the same commit is not validated twice automatically");
        let running = deliver(&v.state, Delivery::Validating);
        assert_eq!(running.state.delivery.as_deref(), Some("validating"));
        // Ask-commands wait for authorization; nothing runs.
        let ask = deliver(&s, Delivery::NeedsAuthorization { commands: 2 });
        assert_eq!((ask.state.status, &ask.action), (WorkflowStatus::Waiting, &Action::None));
        // A passed validation never integrates by itself: the operator approves.
        let ready = deliver(&running.state, Delivery::AwaitingApproval { validated: true });
        assert_eq!((ready.state.status, &ready.action), (WorkflowStatus::Waiting, &Action::None));
        let integrating = deliver(&ready.state, Delivery::Integrating);
        assert_eq!(integrating.state.status, WorkflowStatus::Running);
        let done = deliver(
            &integrating.state,
            Delivery::Integrated { merge_commit: "m".into(), target_branch: "main".into() },
        );
        assert_eq!((done.state.status, &done.action), (WorkflowStatus::Delivered, &Action::CompleteTask));
        let late = deliver(&done.state, Delivery::Failed { reason: "x".into() });
        assert_eq!((late.state, late.action), (done.state.clone(), Action::None), "delivered is final");
    }

    #[test]
    fn failed_validation_can_go_back_for_a_correction_or_run_again() {
        let s = delivering();
        let v = deliver(&s, Delivery::NeedsValidation { source: "abc".into() });
        let failed = deliver(&v.state, Delivery::ValidationFailed { reason: "tests failed".into() });
        assert_eq!(failed.state.status, WorkflowStatus::Waiting);
        assert!(failed.state.reason.as_deref().unwrap().contains("tests failed"));
        // Resume runs the validation again.
        let rerun = apply(&failed.state, Input::Resume);
        assert_eq!(rerun.action, Action::Validate);
        // Or the operator sends it back: a correction round, then review, then delivery again.
        let c = apply(&failed.state, Input::RequestCorrection);
        assert_eq!(key(&c), "implement#2");
        let r = report(&c, Outcome::Done);
        assert_eq!(key(&r), "review#2");
        let back = report(&r, Outcome::Review(ReviewResult::Approved));
        assert_eq!(back.action, Action::Prepare);
        // Delivery facts are ignored while the work is being corrected.
        let ignored = deliver(&c.state, Delivery::AwaitingApproval { validated: true });
        assert_eq!(ignored.action, Action::None);
        assert_eq!(ignored.state, c.state);
        // No correction while integrating.
        let integrating = deliver(&s, Delivery::Integrating);
        assert!(decide(&integrating.state, &Input::RequestCorrection).is_err());
    }

    #[test]
    fn blocked_failed_and_cancelled_deliveries_wait_and_resume_prepares_again() {
        let s = delivering();
        for fact in [
            Delivery::Blocked { reason: "the target branch moved".into() },
            Delivery::Failed { reason: "ref changed".into() },
            Delivery::Cancelled,
        ] {
            let w = deliver(&s, fact);
            assert_eq!(w.state.status, WorkflowStatus::Waiting);
            assert_eq!(apply(&w.state, Input::Resume).action, Action::Prepare);
        }
        // An M8.1-era state without delivery fields still deserializes.
        let old: WorkflowState =
            serde_json::from_str(r#"{"status":"completed","stage":"delivery"}"#).unwrap();
        assert_eq!(old.status, WorkflowStatus::Completed);
    }

    /// A workflow started before review triage (no `review_triage` in its
    /// stored state) keeps the direct review → correction routing.
    fn without_triage() -> Decision {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d.state.review_triage = false;
        d
    }

    #[test]
    fn correction_rounds_are_bounded_then_the_operator_decides() {
        let mut d = without_triage();
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
        // A stored state from before triage deserializes without it.
        let old: WorkflowState = serde_json::from_str(r#"{"status":"running","stage":"review"}"#).unwrap();
        assert!(!old.review_triage);
    }

    #[test]
    fn reasoning_can_dispute_findings_and_skip_implementation() {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d = report(&d, Outcome::Done); // implement#1
        d = report(&d, Outcome::Done); // review#1
        d = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(key(&d), "investigate#2");
        // Dispute: straight back to review, no implementation in between.
        d = report(&d, Outcome::Triage(TriageDecision::Dispute));
        assert_eq!(key(&d), "review#2");
        assert_eq!(d.state.stage, Some(Stage::Review));
        // The reviewer still disagrees; reasoning now agrees and implementation fixes it.
        d = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(key(&d), "investigate#3");
        d = report(&d, Outcome::Triage(TriageDecision::Fix));
        assert_eq!(key(&d), "implement#3");
        d = report(&d, Outcome::Done);
        assert_eq!(key(&d), "review#3");
        // Two rounds used: a person breaks the tie.
        let waiting = report(&d, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(waiting.state.status, WorkflowStatus::Waiting);
        assert!(waiting.state.reason.as_deref().unwrap().contains("rounds of fixes or reasoning"));
        assert_eq!(key(&apply(&waiting.state, Input::Resume)), "investigate#4");
        // A reasoning report without a decision is treated as a fix.
        let mut e = apply(&WorkflowState::default(), Input::Start);
        e = report(&e, Outcome::Done);
        e = report(&e, Outcome::Done);
        e = report(&e, Outcome::Review(ReviewResult::ChangesRequested));
        assert_eq!(key(&report(&e, Outcome::Done)), "implement#2");
        // The first investigation is never a triage, whatever it reports.
        let s = apply(&WorkflowState::default(), Input::Start);
        assert_eq!(key(&report(&s, Outcome::Triage(TriageDecision::Dispute))), "implement#1");
    }

    #[test]
    fn rejected_and_blocked_reviews_and_blockers_stop_for_the_operator() {
        let mut d = apply(&WorkflowState::default(), Input::Start);
        d = report(&d, Outcome::Done);
        d = report(&d, Outcome::Done);
        let rejected = report(&d, Outcome::Review(ReviewResult::Rejected));
        // A rejection waits for a person, with or without triage.
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
    fn directed_work_starts_a_new_round_only_while_no_agent_works() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let directed = |s: &WorkflowState, target| decide(s, &Input::Directed { target });
        // investigate#1 is working: nothing is redirected underneath it.
        assert!(matches!(directed(&d.state, Target::Coding), Err(WorkflowError::Busy(_))));
        // Waiting for an answer: still the agent's turn.
        let q = apply(&d.state, Input::Question { step: "investigate#1".into(), summary: "x?".into() });
        assert!(matches!(directed(&q.state, Target::Review), Err(WorkflowError::Busy(_))));
        let imp = report(&d, Outcome::Done);
        let tests = apply(&imp.state, Input::Reported { step: "implement#1".into(), outcome: Outcome::Done });
        assert_eq!(tests.action, Action::RunTests { authorized: false });
        assert!(
            matches!(directed(&tests.state, Target::Review), Err(WorkflowError::Busy(_))),
            "tests are running"
        );
        // A review that ended without a verdict waits for a retry: the operator may send it elsewhere.
        let rev = apply(
            &tests.state,
            Input::Tested { step: "implement#1".into(), result: Tested::Passed("ok".into()) },
        );
        let stuck = report(&rev, Outcome::Done);
        assert_eq!(stuck.state.status, WorkflowStatus::Waiting);
        let to_reasoning = directed(&stuck.state, Target::Reasoning).unwrap();
        assert_eq!(key(&to_reasoning), "investigate#2");
        assert!(is_triage_step("investigate#2"));
        assert!(to_reasoning.changes.contains(&Change::Directed(Target::Reasoning)));
        assert_eq!(key(&directed(&stuck.state, Target::Coding).unwrap()), "implement#2");
        assert_eq!(key(&directed(&stuck.state, Target::Review).unwrap()), "review#2");
        // The tests run only for a finished implementation.
        assert!(directed(&stuck.state, Target::Tests).is_err());
        let mut after_impl = tests.state.clone();
        after_impl.status = WorkflowStatus::Waiting;
        after_impl.on_resume = Some(Resume::RunTests { authorized: true });
        assert_eq!(
            directed(&after_impl, Target::Tests).unwrap().action,
            Action::RunTests { authorized: false }
        );
        // Ended workflows are not redirected.
        let c = apply(&stuck.state, Input::Cancel { reason: "x".into() });
        assert!(directed(&c.state, Target::Coding).is_err());
    }

    #[test]
    fn questions_interruptions_cancel_and_closed_tasks() {
        let d = apply(&WorkflowState::default(), Input::Start);
        let q =
            apply(&d.state, Input::Question { step: "investigate#1".into(), summary: "which file?".into() });
        assert_eq!(q.state.status, WorkflowStatus::Waiting);
        assert_eq!(q.state.on_resume, Some(Resume::Answer { question: "which file?".into() }));
        assert_eq!(
            q.state.reason.as_deref(),
            Some(
                "investigate#1 asks you: which file? Answer it; the workflow continues when the agent reports."
            )
        );
        // Resuming without an answer is refused: the agent is still waiting.
        assert_eq!(decide(&q.state, &Input::Resume), Err(WorkflowError::NeedsAnswer));
        // The answer reached the agent: the workflow runs again, nothing new starts.
        let answered = apply(&q.state, Input::Answered { step: "investigate#1".into() });
        assert_eq!((answered.state.status, &answered.action), (WorkflowStatus::Running, &Action::None));
        assert_eq!(answered.changes, vec![Change::Resumed]);
        // A note about another step, or with nothing asked, changes nothing.
        let other = apply(&q.state, Input::Answered { step: "implement#1".into() });
        assert_eq!(other.state, q.state);
        let unasked = apply(&d.state, Input::Answered { step: "investigate#1".into() });
        assert_eq!((unasked.state, unasked.changes), (d.state.clone(), vec![]));
        // A question stored before `Answer` existed is answered the same way.
        let mut legacy = q.state.clone();
        legacy.on_resume = Some(Resume::Continue);
        assert_eq!(
            apply(&legacy, Input::Answered { step: "investigate#1".into() }).state.status,
            WorkflowStatus::Running
        );
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
        assert_eq!(Outcome::from_assignment("triage: fix", None), Some(Outcome::Triage(TriageDecision::Fix)));
        assert_eq!(
            Outcome::from_assignment("triage: dispute", None),
            Some(Outcome::Triage(TriageDecision::Dispute))
        );
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
