//! Dynamic agent organizations (ADR 0035), as a pure decision function.
//!
//! An organization is created for one objective (a root task). It holds
//! **agents** (supervisors and specialists, in a tree of reporting lines with
//! no fixed depth) and **organization tasks** (the root objective, coordination
//! tasks owned by supervisors, and work owned by specialists). The runtime
//! (provider sessions) is Core's: this module only decides who works or
//! supervises next, and what a supervisor's directive changes.
//!
//! Three things stay separate:
//! * the organization ([`Agent`]: identity, capabilities, responsibility,
//!   scope, reporting line, provider/model),
//! * the task graph ([`OrgTask`], each work or coordination task backed by an
//!   M6 task, with dependencies and acceptance criteria),
//! * the runtime ([`Activity`]: at most one session at a time per
//!   organization workspace, chosen by Core).
//!
//! Core stores an [`OrgState`], turns events into an [`Input`], calls
//! [`decide`], commits the new state together with its journal events, new
//! subtasks and context entries, and only then performs the [`Action`].
//! Nothing here performs I/O or knows a provider.

mod directive;
pub mod goal;
mod render;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use directive::{
    AgentRequest, Assessment, ContextNote, Directive, DirectiveError, EscalateTo, Escalation, FENCE,
    MAX_DIRECTIVE_BYTES, TaskRequest, Verdict, parse_directive, valid_key, valid_scope,
};
pub use render::{ContextEntry, OrgBrief, current, render_supervisor, supervisor_marker, worker_brief};

use crate::model::{ReviewResult, Role, TaskKind};

pub const DEFINITION: &str = "organization";
pub const VERSION: u32 = 1;
/// Turns a supervisor may retry after an invalid directive before a person decides.
pub const MAX_REPAIRS: u32 = 2;
/// Key of the root objective task and of the first supervisor.
pub const ROOT: &str = "objective";
pub const LEAD: &str = "lead";

macro_rules! snake_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $s),+ } }
            pub fn parse(s: &str) -> Option<Self> { match s { $($s => Some(Self::$variant),)+ _ => None } }
        }
    };
}

snake_enum!(
    /// Whether an agent supervises others or does the work itself.
    AgentKind { Supervisor => "supervisor", Specialist => "specialist" }
);

snake_enum!(
    /// What an organization task is. Only `coordination` is owned by a
    /// supervisor; everything else is executed by a specialist.
    WorkKind {
        Coordination => "coordination",
        Implementation => "implementation",
        Investigation => "investigation",
        Documentation => "documentation",
        Testing => "testing",
        Review => "review",
    }
);

impl WorkKind {
    /// The M6 assignment role a specialist runs this work as (`None`: coordination).
    pub fn role(self) -> Option<Role> {
        match self {
            Self::Coordination => None,
            Self::Investigation => Some(Role::Investigate),
            Self::Implementation | Self::Documentation | Self::Testing => Some(Role::Implement),
            Self::Review => Some(Role::Review),
        }
    }

    /// The M6 task kind of the backing task.
    pub fn task_kind(self) -> TaskKind {
        match self {
            Self::Investigation => TaskKind::Investigation,
            Self::Implementation | Self::Testing => TaskKind::Implementation,
            Self::Documentation | Self::Review | Self::Coordination => TaskKind::General,
        }
    }

    /// Whether the work may change files (and so has a scope to stay in).
    pub fn changes_files(self) -> bool {
        matches!(self.role(), Some(Role::Implement | Role::Investigate))
    }
}

snake_enum!(
    AgentStatus {
        /// Waiting for work.
        Idle => "idle",
        /// Its session is running (a work turn or a supervision turn).
        Active => "active",
        /// Everything it owned is finished.
        Done => "done",
    }
);

snake_enum!(
    TaskState {
        /// A coordination task its supervisor has not broken down yet.
        Planning => "planning",
        /// A coordination task whose subtasks are under way.
        Delegated => "delegated",
        /// Work waiting for its dependencies.
        Waiting => "waiting",
        Ready => "ready",
        Working => "working",
        /// Work reported (or ended); its supervisor reviews it next.
        Reported => "reported",
        /// New work outside the agreed scope: it waits for the user (ADR 0046).
        Proposed => "proposed",
        Accepted => "accepted",
        Cancelled => "cancelled",
    }
);

impl TaskState {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Accepted | Self::Cancelled)
    }
}

snake_enum!(
    OrgStatus {
        Running => "running",
        /// A person decides: an escalation, a limit, or nothing can run.
        Waiting => "waiting",
        Paused => "paused",
        /// The lead reported the objective finished; delivery follows.
        Done => "done",
        Cancelled => "cancelled",
    }
);

snake_enum!(
    /// What an entry of the shared engineering context is.
    ContextKind {
        Objective => "objective",
        Criterion => "criterion",
        Decision => "decision",
        Finding => "finding",
        Risk => "risk",
        Instruction => "instruction",
        Deliverable => "deliverable",
        Progress => "progress",
        Disagreement => "disagreement",
        Observation => "observation",
        Escalation => "escalation",
    }
);

snake_enum!(
    /// How much an entry can be relied on.
    ContextStatus {
        /// Established by Switchyard itself (from Git, the journal, the user).
        Verified => "verified",
        /// Stated by an agent; not checked.
        Claim => "claim",
        /// Suggested, not decided.
        Proposal => "proposal",
        /// Decided by a supervisor within its authority.
        Decided => "decided",
        /// Replaced by a later entry.
        Superseded => "superseded",
    }
);

/// Operational limits (configurable per project; Core rejects values the
/// runtime cannot honour). They bound the organization, not the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Levels of reporting below the user (the lead is depth 1).
    pub max_depth: u32,
    pub max_agents: u32,
    /// Organization tasks, the objective included.
    pub max_tasks: u32,
    /// Sessions running at the same time. The runtime supports 1 per
    /// organization workspace (ADR 0036).
    pub max_concurrent_sessions: u32,
    /// Correction rounds per task.
    pub max_corrections: u32,
    /// Automatic retries of a work turn that failed or had no usable report.
    pub max_retries: u32,
    /// Supervision turns in total.
    pub max_supervision_turns: u32,
    /// Wall-clock minutes from the start.
    pub max_minutes: u32,
    /// Spend, where the provider reports cost.
    pub max_cost_usd: Option<f64>,
    /// Tokens, where the provider reports usage.
    pub max_tokens: Option<u64>,
    /// Work turns on one task before the user is asked (ADR 0046).
    pub max_task_turns: u32,
    /// Supervision turns in a row without accepted work before the user is asked.
    pub max_turns_without_progress: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_agents: 6,
            max_tasks: 12,
            max_concurrent_sessions: 1,
            max_corrections: 2,
            max_retries: 1,
            max_supervision_turns: 30,
            max_minutes: 240,
            max_cost_usd: None,
            max_tokens: None,
            max_task_turns: 3,
            max_turns_without_progress: 8,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), String> {
        let range = |name: &str, v: u32, lo: u32, hi: u32| {
            if (lo..=hi).contains(&v) { Ok(()) } else { Err(format!("{name} must be between {lo} and {hi}")) }
        };
        range("max_depth", self.max_depth, 1, 8)?;
        range("max_agents", self.max_agents, 1, 32)?;
        range("max_tasks", self.max_tasks, 2, 64)?;
        range("max_corrections", self.max_corrections, 0, 10)?;
        range("max_retries", self.max_retries, 0, 5)?;
        range("max_supervision_turns", self.max_supervision_turns, 1, 200)?;
        range("max_minutes", self.max_minutes, 1, 24 * 60)?;
        range("max_task_turns", self.max_task_turns, 1, 20)?;
        range("max_turns_without_progress", self.max_turns_without_progress, 1, 100)?;
        if self.max_concurrent_sessions != 1 {
            return Err("max_concurrent_sessions must be 1: an organization works in one workspace, and a \
                        workspace runs one session at a time"
                .into());
        }
        if self.max_cost_usd.is_some_and(|c| !(c > 0.0 && c.is_finite())) {
            return Err("max_cost_usd must be positive".into());
        }
        if self.max_tokens == Some(0) {
            return Err("max_tokens must be positive".into());
        }
        Ok(())
    }
}

/// A provider/model Core selected for an agent, with its reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selected {
    pub provider: String,
    pub model: Option<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Core-assigned identity (stable across restarts and replacements).
    pub id: Uuid,
    pub key: String,
    pub title: String,
    pub kind: AgentKind,
    /// `None` only for the lead (reports to the user).
    pub reports_to: Option<String>,
    pub responsibility: String,
    pub scope: Vec<String>,
    pub capabilities: Vec<String>,
    pub provider: String,
    pub model: Option<String>,
    pub selection: String,
    pub status: AgentStatus,
    /// `user`, `core` or the key of the supervisor that added it.
    pub created_by: String,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgTask {
    pub key: String,
    /// The backing M6 task (set by Core when it creates it).
    pub task_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub work: WorkKind,
    /// Coordination task this belongs to (`None` for the objective).
    pub parent: Option<String>,
    pub owner: String,
    pub scope: Vec<String>,
    pub acceptance: Vec<String>,
    pub depends_on: Vec<String>,
    pub state: TaskState,
    pub corrections: u32,
    pub attempts: u32,
    /// Work turns started on this task (the loop guard, ADR 0046).
    #[serde(default)]
    pub turns: u32,
    /// A coordination area added after the plan was agreed: its own breakdown
    /// is not agreed scope (its implementation work needs `continues` or the
    /// user's approval).
    #[serde(default)]
    pub late: bool,
    /// Supervisor instructions for the owner's next turn.
    pub instructions: Vec<String>,
    /// The instructions of the turn that runs now (shown in its context).
    #[serde(default)]
    pub current_instructions: Vec<String>,
    /// The latest report or acceptance summary.
    pub summary: Option<String>,
    pub created_by: String,
}

/// What Core verified about a work turn (from Git and its own records,
/// never from the agent).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Evidence {
    /// Workspace files that differ from the start of the organization.
    pub changed_files: Vec<String>,
    /// Of those, files outside the task's scope (and not owned by accepted work).
    pub outside_scope: Vec<String>,
}

/// How a work turn ended, from Core's records of the assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkerOutcome {
    /// Implementation or findings reported.
    Delivered {
        summary: String,
    },
    Reviewed {
        result: ReviewResult,
        summary: String,
    },
    /// The specialist disagrees or proposes another approach (a `proposal` report).
    Challenge {
        summary: String,
        details: Option<String>,
    },
    Blocker {
        summary: String,
    },
    Question {
        summary: String,
    },
    /// No usable report, a failed session, or a turn ended without a report.
    Failed {
        reason: String,
    },
    /// Core interrupted the turn (an observation); the supervisor reviews it.
    Interrupted {
        reason: String,
    },
}

impl WorkerOutcome {
    pub fn summary(&self) -> &str {
        match self {
            Self::Delivered { summary }
            | Self::Reviewed { summary, .. }
            | Self::Challenge { summary, .. }
            | Self::Blocker { summary }
            | Self::Question { summary } => summary,
            Self::Failed { reason } | Self::Interrupted { reason } => reason,
        }
    }
}

/// Something Core noticed while a specialist worked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Observation {
    /// The specialist started (or asked) to change files outside its scope.
    OutsideScope { paths: Vec<String> },
    /// Several tool operations failed in one turn.
    RepeatedFailures { count: u32 },
}

impl Observation {
    pub fn describe(&self) -> String {
        match self {
            Self::OutsideScope { paths } => {
                format!("started to change files outside its assigned scope: {}", paths.join(", "))
            }
            Self::RepeatedFailures { count } => format!("had {count} failed tool operations in one turn"),
        }
    }
}

/// One thing a supervisor is asked to look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CheckpointItem {
    /// Break the coordination task down.
    Plan,
    /// Work in the supervisor's scope reported.
    Reported { task: String, outcome: WorkerOutcome, evidence: Evidence },
    /// Core observed something during a work turn (and interrupted it).
    Observed { task: String, text: String },
    /// A subordinate supervisor escalated.
    Escalated { from: String, question: String },
    /// The user answered this supervisor's escalation.
    Answer { text: String },
    /// Its last directive was rejected.
    Rejected { error: String },
    /// Its own supervisor sent its finished scope back with instructions.
    Instruction { from: String, text: String },
    /// Every subtask of the coordination task is accepted or cancelled.
    ChildrenFinished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub supervisor: String,
    /// The coordination task the supervision is about.
    pub task: String,
    pub items: Vec<CheckpointItem>,
}

/// What runs now. One session at a time per organization.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Activity {
    #[default]
    Idle,
    Supervising {
        checkpoint: Checkpoint,
    },
    Working {
        task: String,
        agent: String,
        interrupting: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgState {
    pub status: OrgStatus,
    pub reason: Option<String>,
    pub limits: Limits,
    pub agents: Vec<Agent>,
    pub tasks: Vec<OrgTask>,
    pub queue: Vec<Checkpoint>,
    pub activity: Activity,
    /// Invalid directives in a row from the current supervisor.
    pub repairs: u32,
    pub supervision_turns: u32,
    pub started_at_ms: i64,
    pub cost_usd: f64,
    pub tokens: u64,
    /// The supervisor whose escalation waits for the user's answer.
    pub question_from: Option<String>,
    pub final_summary: Option<String>,
    /// Runtime bookkeeping (Core): each supervisor's session, resumed at its
    /// next checkpoint so it keeps its conversation.
    #[serde(default)]
    pub sessions: BTreeMap<String, Uuid>,
    /// Journal position before the current supervision turn's prompt.
    #[serde(default)]
    pub turn_after_seq: i64,
    /// Waiting tasks whose supervisor was told they depend on cancelled
    /// work (told once each; after that the user decides).
    #[serde(default)]
    pub dead_ends_reported: Vec<String>,
    /// The first plan was made: later new work must continue a task or wait
    /// for the user's approval (ADR 0046).
    #[serde(default)]
    pub scope_agreed: bool,
    /// Supervision turns since work was last accepted (the loop guard).
    #[serde(default)]
    pub turns_since_progress: u32,
    /// The latest goal check (ADR 0046, stage 2).
    #[serde(default)]
    pub goal: Option<goal::GoalReport>,
    /// Work was accepted since the last goal check.
    #[serde(default)]
    pub goal_due: bool,
    #[serde(default)]
    pub last_goal_check_ms: i64,
    /// Supervision turns in a row that failed without a directive.
    #[serde(default)]
    pub supervision_failures: u32,
}

/// An entry for the shared context, written by Core with the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewContext {
    pub kind: ContextKind,
    pub status: ContextStatus,
    pub text: String,
    pub task: Option<String>,
    pub agent: Option<String>,
    /// `core`, `user` or `agent:<key>`.
    pub source: String,
    pub supersedes: Option<i64>,
}

/// Something that happened, as the organization sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Create the organization: the objective owned by the lead supervisor.
    Start {
        objective: Box<OrgTask>,
        lead: Box<Agent>,
        now_ms: i64,
    },
    /// A supervision turn ended. `selections` holds Core's provider/model
    /// choice (or refusal) for every agent the directive requests.
    Supervised {
        agent: String,
        directive: Result<Directive, String>,
        selections: BTreeMap<String, Result<Selected, String>>,
        new_ids: BTreeMap<String, Uuid>,
        now_ms: i64,
    },
    /// A supervision turn failed without a directive (its provider's usage
    /// limit, an API error): not a badly written directive. Retried within
    /// `max_retries`, then the user is told the cause.
    SupervisionFailed {
        agent: String,
        reason: String,
        now_ms: i64,
    },
    /// A work turn ended.
    Worked {
        task: String,
        outcome: WorkerOutcome,
        evidence: Evidence,
        now_ms: i64,
    },
    /// Core noticed something during a work turn.
    Observed {
        task: String,
        observation: Observation,
    },
    /// Usage of a finished turn, when the provider reports it.
    Usage {
        cost_usd: Option<f64>,
        tokens: Option<u64>,
    },
    /// The running session ended without a result (a restart, a crash).
    Interrupted {
        reason: String,
    },
    Tick {
        now_ms: i64,
    },
    Pause,
    Resume {
        now_ms: i64,
    },
    Cancel {
        reason: String,
    },
    Answer {
        text: String,
        now_ms: i64,
    },
    /// Limits changed (the user raised them in the project settings).
    SetLimits(Limits),
    /// Delivery sent the finished objective back (a failed validation, a
    /// blocker): the lead supervisor continues with the user's note.
    Reopen {
        text: String,
        now_ms: i64,
    },
    /// The user cancelled one of the organization's tasks outside it (its M6
    /// task was cancelled or failed): it and what is open under it are
    /// cancelled, and its supervisor decides what happens next.
    TaskClosed {
        task: String,
        status: String,
        now_ms: i64,
    },
    /// The independent goal check's verdict (ADR 0046, stage 2).
    GoalChecked {
        report: goal::GoalReport,
        now_ms: i64,
    },
    /// The user approves or declines work proposed outside the agreed scope (ADR 0046).
    ScopeDecision {
        key: String,
        approve: bool,
        now_ms: i64,
    },
    /// Switchyard's watchdog (ADR 0045) found the team stuck (its resumes
    /// keep stopping at the same place): the lead decides how to continue.
    Nudge {
        text: String,
        now_ms: i64,
    },
}

/// What Core must do after committing the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Start (or resume) the supervisor's session with this checkpoint.
    Supervise {
        checkpoint: Checkpoint,
    },
    /// Start a work turn: an M6 assignment of the task to its owner, with the
    /// supervisor's instructions in its context.
    Work {
        task: String,
        agent: String,
        instructions: Vec<String>,
    },
    /// Interrupt the running work turn at once (an observation).
    Interrupt {
        task: String,
    },
    /// Stop whatever runs (the organization was cancelled).
    Stop,
    /// The objective is finished: hand it to delivery (M7, the user's approval).
    Deliver {
        summary: String,
    },
}

/// A journal-worthy change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Created,
    AgentAdded {
        key: String,
    },
    TaskDelegated {
        key: String,
    },
    /// New work outside the agreed scope waits for the user (ADR 0046).
    ScopeProposed {
        key: String,
    },
    /// The independent goal check judged the team (ADR 0046).
    GoalChecked {
        verdict: goal::Verdict,
        summary: String,
    },
    Assessed {
        task: String,
        verdict: Verdict,
        by: String,
        text: Option<String>,
    },
    Observed {
        task: String,
        text: String,
    },
    Escalated {
        by: String,
        to: EscalateTo,
        question: String,
    },
    DirectiveRejected {
        by: String,
        error: String,
    },
    Waiting {
        reason: String,
    },
    Paused,
    Resumed,
    Done {
        summary: String,
    },
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub state: OrgState,
    pub action: Action,
    pub changes: Vec<Change>,
    pub context: Vec<NewContext>,
    /// Organization tasks created by this decision (Core creates their M6 tasks).
    pub created_tasks: Vec<String>,
    /// Tasks whose M6 task Core must complete (`true`) or cancel (`false`).
    pub closed_tasks: Vec<(String, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrgError {
    #[error("the organization is {0}")]
    NotAllowed(&'static str),
}

/// `path` lies in `scope` (an entry is a directory or file; `.` is everything).
pub fn in_scope(path: &str, scope: &[String]) -> bool {
    let path = path.trim_start_matches("./");
    scope.iter().any(|s| {
        let s = s.trim().trim_start_matches("./").trim_end_matches('/');
        s.is_empty() || s == "." || path == s || path.starts_with(&format!("{s}/"))
    })
}

fn norm_title(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

impl OrgState {
    pub fn agent(&self, key: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.key == key)
    }
    fn agent_mut(&mut self, key: &str) -> Option<&mut Agent> {
        self.agents.iter_mut().find(|a| a.key == key)
    }
    pub fn task(&self, key: &str) -> Option<&OrgTask> {
        self.tasks.iter().find(|t| t.key == key)
    }
    fn task_mut(&mut self, key: &str) -> Option<&mut OrgTask> {
        self.tasks.iter_mut().find(|t| t.key == key)
    }
    pub fn task_by_id(&self, id: Uuid) -> Option<&OrgTask> {
        self.tasks.iter().find(|t| t.task_id == Some(id))
    }
    pub fn agent_by_id(&self, id: Uuid) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// `key` is `boss` or reports to `boss`, directly or indirectly.
    pub fn in_subtree(&self, key: &str, boss: &str) -> bool {
        let mut cur = Some(key.to_owned());
        let mut hops = 0;
        while let Some(k) = cur {
            if k == boss {
                return true;
            }
            hops += 1;
            if hops > self.agents.len() + 1 {
                return false;
            }
            cur = self.agent(&k).and_then(|a| a.reports_to.clone());
        }
        false
    }

    /// The supervisor that reviews `task`: the owner of its coordination task.
    pub fn reviewer_of(&self, task: &str) -> Option<String> {
        let t = self.task(task)?;
        match &t.parent {
            Some(p) => self.task(p).map(|p| p.owner.clone()),
            None => None,
        }
    }

    pub fn children(&self, parent: &str) -> impl Iterator<Item = &OrgTask> {
        self.tasks.iter().filter(move |t| t.parent.as_deref() == Some(parent))
    }

    fn enqueue(&mut self, supervisor: &str, task: &str, item: CheckpointItem) {
        if let Some(c) = self.queue.iter_mut().find(|c| c.supervisor == supervisor && c.task == task) {
            c.items.push(item);
        } else {
            self.queue.push(Checkpoint {
                supervisor: supervisor.into(),
                task: task.into(),
                items: vec![item],
            });
        }
    }

    /// The coordination task a supervisor's checkpoints default to.
    fn home_task(&self, supervisor: &str) -> Option<String> {
        self.tasks
            .iter()
            .find(|t| t.owner == supervisor && t.work == WorkKind::Coordination && !t.state.is_final())
            .or_else(|| self.tasks.iter().find(|t| t.owner == supervisor && t.work == WorkKind::Coordination))
            .map(|t| t.key.clone())
    }
}

fn ctx(
    kind: ContextKind,
    status: ContextStatus,
    text: String,
    task: Option<&str>,
    source: &str,
) -> NewContext {
    NewContext {
        kind,
        status,
        text,
        task: task.map(str::to_owned),
        agent: None,
        source: source.into(),
        supersedes: None,
    }
}

/// Decides what happens next. Inputs that do not match what is running
/// (replays, stale events) change nothing.
pub fn decide(state: &OrgState, input: &Input) -> Result<Decision, OrgError> {
    let mut d = Decision {
        state: state.clone(),
        action: Action::None,
        changes: Vec::new(),
        context: Vec::new(),
        created_tasks: Vec::new(),
        closed_tasks: Vec::new(),
    };
    let finished = matches!(state.status, OrgStatus::Done | OrgStatus::Cancelled);
    match input {
        Input::Start { objective, lead, now_ms } => {
            if !state.agents.is_empty() {
                return Err(OrgError::NotAllowed("already started"));
            }
            d.state.started_at_ms = *now_ms;
            d.state.status = OrgStatus::Running;
            d.state.agents.push(lead.as_ref().clone());
            let mut root = objective.as_ref().clone();
            root.state = TaskState::Planning;
            d.context.push(ctx(
                ContextKind::Objective,
                ContextStatus::Verified,
                format!(
                    "{}{}",
                    root.title,
                    root.description.as_deref().map(|s| format!(": {s}")).unwrap_or_default()
                ),
                Some(&root.key),
                "user",
            ));
            for c in &root.acceptance {
                d.context.push(ctx(
                    ContextKind::Criterion,
                    ContextStatus::Verified,
                    c.clone(),
                    Some(&root.key),
                    "user",
                ));
            }
            d.state.tasks.push(root);
            d.changes.push(Change::Created);
            d.changes.push(Change::AgentAdded { key: lead.key.clone() });
            schedule(&mut d, *now_ms);
        }
        Input::Reopen { text, now_ms } => {
            if state.status != OrgStatus::Done {
                return Err(OrgError::NotAllowed("not finished"));
            }
            d.state.status = OrgStatus::Running;
            d.state.reason = None;
            d.state.final_summary = None;
            let has_children = d.state.children(ROOT).next().is_some();
            if let Some(root) = d.state.task_mut(ROOT) {
                root.state = if has_children { TaskState::Delegated } else { TaskState::Planning };
            }
            if let Some(a) = d.state.agent_mut(LEAD) {
                a.status = AgentStatus::Idle;
            }
            d.state.enqueue(
                LEAD,
                ROOT,
                CheckpointItem::Instruction { from: "the user".into(), text: text.clone() },
            );
            d.context.push(ctx(
                ContextKind::Instruction,
                ContextStatus::Verified,
                format!("The user sent the result back from delivery: {text}"),
                Some(ROOT),
                "user",
            ));
            d.changes.push(Change::Resumed);
            schedule(&mut d, *now_ms);
        }
        _ if finished => return Ok(d),
        Input::Supervised { agent, directive, selections, new_ids, now_ms } => {
            let Activity::Supervising { checkpoint } = &state.activity else { return Ok(d) };
            if checkpoint.supervisor != *agent {
                return Ok(d);
            }
            let checkpoint = checkpoint.clone();
            d.state.activity = Activity::Idle;
            if let Some(a) = d.state.agent_mut(agent) {
                a.status = AgentStatus::Idle;
            }
            let applied = match directive {
                Ok(dir) => apply_directive(&mut d, &checkpoint, dir, selections, new_ids),
                Err(e) => Err(e.clone()),
            };
            d.state.supervision_failures = 0;
            match applied {
                Ok(()) => d.state.repairs = 0,
                Err(error) => {
                    d.changes.push(Change::DirectiveRejected { by: agent.clone(), error: error.clone() });
                    d.state.repairs += 1;
                    let mut retry = checkpoint.clone();
                    retry.items.retain(|i| !matches!(i, CheckpointItem::Rejected { .. }));
                    retry.items.push(CheckpointItem::Rejected { error: error.clone() });
                    d.state.queue.insert(0, retry);
                    if d.state.repairs > MAX_REPAIRS {
                        let n = d.state.repairs;
                        wait(
                            &mut d,
                            format!(
                                "{}'s directive was rejected {n} times in a row (last: {error}). Resume to let it try again.",
                                title_of(state, agent),
                            ),
                        );
                        d.state.repairs = 0;
                    }
                }
            }
            schedule(&mut d, *now_ms);
        }
        Input::SupervisionFailed { agent, reason, now_ms } => {
            let Activity::Supervising { checkpoint } = &state.activity else { return Ok(d) };
            if checkpoint.supervisor != *agent {
                return Ok(d);
            }
            let checkpoint = checkpoint.clone();
            d.state.activity = Activity::Idle;
            if let Some(a) = d.state.agent_mut(agent) {
                a.status = AgentStatus::Idle;
            }
            // The same checkpoint is asked again: nothing in it was handled.
            d.state.queue.insert(0, checkpoint);
            d.state.supervision_failures += 1;
            let text = format!("{}'s turn failed: {reason}", title_of(state, agent));
            d.changes.push(Change::Observed { task: ROOT.into(), text: text.clone() });
            d.context.push(ctx(
                ContextKind::Observation,
                ContextStatus::Verified,
                text.clone(),
                None,
                "core",
            ));
            if d.state.supervision_failures > d.state.limits.max_retries {
                d.state.supervision_failures = 0;
                wait(
                    &mut d,
                    format!(
                        "{text}. Its provider may be unavailable (a usage limit, an outage). Resume to try again, or \
                         change the lead's provider and start the team again."
                    ),
                );
            }
            schedule(&mut d, *now_ms);
        }
        Input::Worked { task, outcome, evidence, now_ms } => {
            let Activity::Working { task: running, agent, .. } = &state.activity else { return Ok(d) };
            if running != task {
                return Ok(d);
            }
            let agent = agent.clone();
            d.state.activity = Activity::Idle;
            if let Some(a) = d.state.agent_mut(&agent) {
                a.status = AgentStatus::Idle;
            }
            on_worked(&mut d, task, outcome, evidence);
            schedule(&mut d, *now_ms);
        }
        Input::Observed { task, observation } => {
            let Activity::Working { task: running, agent, interrupting } = &state.activity else {
                return Ok(d);
            };
            if running != task || *interrupting {
                return Ok(d);
            }
            let text = format!("{} {}", title_of(state, agent), observation.describe());
            d.state.activity =
                Activity::Working { task: task.clone(), agent: agent.clone(), interrupting: true };
            d.changes.push(Change::Observed { task: task.clone(), text: text.clone() });
            d.context.push(ctx(
                ContextKind::Observation,
                ContextStatus::Verified,
                text.clone(),
                Some(task),
                "core",
            ));
            if let Some(r) = d.state.reviewer_of(task) {
                let home = d.state.task(task).and_then(|t| t.parent.clone()).unwrap_or_default();
                d.state.enqueue(&r, &home, CheckpointItem::Observed { task: task.clone(), text });
            }
            d.action = Action::Interrupt { task: task.clone() };
        }
        Input::TaskClosed { task, status, now_ms } => {
            let Some(t) = state.task(task).cloned() else { return Ok(d) };
            if t.state.is_final()
                || t.parent.is_none()
                || matches!(state.status, OrgStatus::Done | OrgStatus::Cancelled)
            {
                return Ok(d);
            }
            let mut stack = vec![task.clone()];
            let mut stopped = false;
            while let Some(k) = stack.pop() {
                let kids: Vec<String> = d.state.children(&k).map(|c| c.key.clone()).collect();
                if let Some(tt) = d.state.task_mut(&k)
                    && !tt.state.is_final()
                {
                    tt.state = TaskState::Cancelled;
                    // Its own M6 task is already closed; the open ones under it are closed now.
                    if &k != task {
                        d.closed_tasks.push((k.clone(), false));
                    }
                }
                if matches!(&state.activity, Activity::Working { task: running, .. } if *running == k) {
                    stopped = true;
                }
                stack.extend(kids);
            }
            if stopped && let Activity::Working { agent, .. } = &state.activity {
                let agent = agent.clone();
                d.state.activity = Activity::Idle;
                if let Some(a) = d.state.agent_mut(&agent) {
                    a.status = AgentStatus::Idle;
                }
            }
            let text = format!(
                "The user {status} \u{201c}{}\u{201d} outside the organization; decide what happens to the work that depended on it.",
                t.title
            );
            d.changes.push(Change::Observed { task: task.clone(), text: text.clone() });
            d.context.push(ctx(
                ContextKind::Observation,
                ContextStatus::Verified,
                text.clone(),
                Some(task),
                "user",
            ));
            if let Some(r) = d.state.reviewer_of(task) {
                let home = t.parent.clone().unwrap_or_default();
                d.state.enqueue(&r, &home, CheckpointItem::Observed { task: task.clone(), text });
            }
            // A wait that was not a question to the user (a step that could not
            // start, for example on this very task) is over: the supervisor decides.
            if state.status == OrgStatus::Waiting && state.question_from.is_none() {
                d.state.status = OrgStatus::Running;
                d.state.reason = None;
                d.changes.push(Change::Resumed);
            }
            if d.state.status == OrgStatus::Running {
                schedule(&mut d, *now_ms);
            }
        }
        Input::GoalChecked { report, now_ms } => {
            if matches!(state.status, OrgStatus::Done | OrgStatus::Cancelled) {
                return Ok(d);
            }
            let mut report = report.clone();
            report.at_ms = *now_ms;
            d.state.goal = Some(report.clone());
            d.state.goal_due = false;
            d.state.last_goal_check_ms = *now_ms;
            d.changes.push(Change::GoalChecked { verdict: report.verdict, summary: report.summary.clone() });
            d.context.push(ctx(
                ContextKind::Progress,
                ContextStatus::Claim,
                format!("Goal check ({}): {}", report.verdict.as_str().replace('_', " "), report.summary),
                None,
                "goal-check",
            ));
            match report.verdict {
                goal::Verdict::OnTrack => {}
                goal::Verdict::Done => {
                    if let Some(lead) =
                        d.state.agents.iter().find(|a| a.reports_to.is_none()).map(|a| a.key.clone())
                    {
                        let text = format!(
                            "The independent goal check finds every criterion of the objective met: {} If you agree, \
                             finish now: say \"done\": true. If not, say exactly which criterion is still open and why.",
                            report.summary
                        );
                        // A waiting team keeps its reason: the lead reads this
                        // when the user resumes it.
                        d.state.enqueue(&lead, ROOT, CheckpointItem::Observed { task: ROOT.into(), text });
                        if d.state.status == OrgStatus::Running {
                            schedule(&mut d, *now_ms);
                        }
                    }
                }
                goal::Verdict::Drifting | goal::Verdict::Looping => {
                    // Only a running team is stopped; a waiting one keeps the
                    // reason it waits for (the report is shown either way).
                    if d.state.status == OrgStatus::Running {
                        let off: Vec<String> = report
                            .off_goal
                            .iter()
                            .map(|k| {
                                d.state.task(k).map_or(k.clone(), |t| format!("\u{201c}{}\u{201d}", t.title))
                            })
                            .collect();
                        let what = if report.verdict == goal::Verdict::Drifting {
                            "drifting from"
                        } else {
                            "going in circles on"
                        };
                        wait(
                            &mut d,
                            format!(
                                "The independent goal check finds the team {what} the objective: {}{} Resume to continue \
                                 anyway, tell the lead how to refocus (Ask the supervisor), or stop the team.",
                                report.summary,
                                if off.is_empty() {
                                    String::new()
                                } else {
                                    format!(" Off the objective: {}.", off.join(", "))
                                }
                            ),
                        );
                    }
                }
            }
        }
        Input::ScopeDecision { key, approve, now_ms } => {
            let Some(t) = state.task(key).cloned() else {
                return Err(OrgError::NotAllowed("no such task"));
            };
            if t.state != TaskState::Proposed {
                return Err(OrgError::NotAllowed("the task is not waiting for your approval"));
            }
            let text = if *approve {
                if let Some(tt) = d.state.task_mut(key) {
                    tt.state = TaskState::Waiting;
                }
                format!(
                    "The user approved the new work \u{201c}{}\u{201d}; it is part of the plan now.",
                    t.title
                )
            } else {
                if let Some(tt) = d.state.task_mut(key) {
                    tt.state = TaskState::Cancelled;
                }
                d.closed_tasks.push((key.clone(), false));
                format!(
                    "The user declined the new work \u{201c}{}\u{201d}: stay within the agreed plan and finish the objective.",
                    t.title
                )
            };
            d.context.push(ctx(
                ContextKind::Decision,
                ContextStatus::Decided,
                text.clone(),
                Some(key),
                "user",
            ));
            if let Some(r) = d.state.reviewer_of(key) {
                let home = t.parent.clone().unwrap_or_else(|| ROOT.into());
                d.state.enqueue(&r, &home, CheckpointItem::Observed { task: key.clone(), text });
            }
            if d.state.status == OrgStatus::Waiting && d.state.question_from.is_none() {
                d.state.status = OrgStatus::Running;
                d.state.reason = None;
                d.changes.push(Change::Resumed);
            }
            if d.state.status == OrgStatus::Running {
                schedule(&mut d, *now_ms);
            }
        }
        Input::Nudge { text, now_ms } => {
            if matches!(state.status, OrgStatus::Done | OrgStatus::Cancelled) || state.question_from.is_some()
            {
                return Ok(d);
            }
            let Some(lead) = state.agents.iter().find(|a| a.reports_to.is_none()).map(|a| a.key.clone())
            else {
                return Ok(d);
            };
            d.changes.push(Change::Observed { task: ROOT.into(), text: text.clone() });
            d.context.push(ctx(
                ContextKind::Observation,
                ContextStatus::Verified,
                text.clone(),
                None,
                "core",
            ));
            d.state.enqueue(&lead, ROOT, CheckpointItem::Observed { task: ROOT.into(), text: text.clone() });
            if d.state.status == OrgStatus::Waiting {
                d.state.status = OrgStatus::Running;
                d.state.reason = None;
                d.changes.push(Change::Resumed);
            }
            if d.state.status == OrgStatus::Running {
                schedule(&mut d, *now_ms);
            }
        }
        Input::Usage { cost_usd, tokens } => {
            d.state.cost_usd += cost_usd.unwrap_or(0.0).max(0.0);
            d.state.tokens += tokens.unwrap_or(0);
        }
        Input::Interrupted { reason } => match &state.activity {
            Activity::Idle => {}
            Activity::Supervising { checkpoint } => {
                // Ask the same supervisor again on resume; nothing restarts by itself.
                d.state.queue.insert(0, checkpoint.clone());
                d.state.activity = Activity::Idle;
                if let Some(a) = d.state.agent_mut(&checkpoint.supervisor) {
                    a.status = AgentStatus::Idle;
                }
                wait(&mut d, format!("{reason}. Resume to continue the supervision turn."));
            }
            Activity::Working { task, agent, .. } => {
                if let Some(t) = d.state.task_mut(task) {
                    t.state = TaskState::Ready;
                }
                if let Some(a) = d.state.agent_mut(agent) {
                    a.status = AgentStatus::Idle;
                }
                d.state.activity = Activity::Idle;
                wait(&mut d, format!("{reason}. Resume to run the interrupted work again."));
            }
        },
        Input::Tick { now_ms } => schedule(&mut d, *now_ms),
        Input::Pause => {
            if state.status == OrgStatus::Running {
                d.state.status = OrgStatus::Paused;
                d.state.reason =
                    Some("paused by the user; the current turn finishes, nothing new starts".into());
                d.changes.push(Change::Paused);
            }
        }
        Input::Resume { now_ms } => {
            if matches!(state.status, OrgStatus::Paused | OrgStatus::Waiting) {
                if state.question_from.is_some() {
                    return Err(OrgError::NotAllowed("waiting for your answer"));
                }
                d.state.status = OrgStatus::Running;
                d.state.reason = None;
                // A fresh budget of wall-clock time from now.
                d.state.started_at_ms = *now_ms;
                // The user decided to continue: one more attempt at a task the loop
                // guard held, and a fresh count of turns without progress.
                let max = d.state.limits.max_task_turns;
                for t in d.state.tasks.iter_mut().filter(|t| !t.state.is_final() && t.turns >= max) {
                    t.turns = max.saturating_sub(1);
                }
                d.state.turns_since_progress = 0;
                d.changes.push(Change::Resumed);
                schedule(&mut d, *now_ms);
            }
        }
        Input::Answer { text, now_ms } => {
            let Some(from) = state.question_from.clone() else {
                return Err(OrgError::NotAllowed("not waiting for an answer"));
            };
            let home = d.state.home_task(&from).unwrap_or_else(|| ROOT.into());
            d.state.enqueue(&from, &home, CheckpointItem::Answer { text: text.clone() });
            d.context.push(ctx(
                ContextKind::Decision,
                ContextStatus::Verified,
                format!("The user answered: {text}"),
                None,
                "user",
            ));
            d.state.question_from = None;
            d.state.status = OrgStatus::Running;
            d.state.reason = None;
            d.changes.push(Change::Resumed);
            schedule(&mut d, *now_ms);
        }
        Input::Cancel { reason } => {
            d.state.status = OrgStatus::Cancelled;
            d.state.reason = Some(reason.clone());
            d.changes.push(Change::Cancelled { reason: reason.clone() });
            if d.state.activity != Activity::Idle {
                d.action = Action::Stop;
            }
            d.state.activity = Activity::Idle;
            d.state.queue.clear();
        }
        Input::SetLimits(l) => d.state.limits = l.clone(),
    }
    Ok(d)
}

fn title_of(state: &OrgState, key: &str) -> String {
    state.agent(key).map_or_else(|| key.to_owned(), |a| a.title.clone())
}

fn wait(d: &mut Decision, reason: String) {
    d.state.status = OrgStatus::Waiting;
    d.state.reason = Some(reason.clone());
    d.changes.push(Change::Waiting { reason });
}

fn on_worked(d: &mut Decision, task: &str, outcome: &WorkerOutcome, evidence: &Evidence) {
    let limits = d.state.limits.clone();
    let Some(t) = d.state.task(task).cloned() else { return };
    let retry = matches!(outcome, WorkerOutcome::Failed { .. }) && t.attempts < limits.max_retries;
    if retry {
        if let Some(t) = d.state.task_mut(task) {
            t.attempts += 1;
            t.state = TaskState::Ready;
            t.instructions.push(format!(
                "Your previous turn on this task ended without a usable result ({}). Continue the task and end \
                 with the report block.",
                outcome.summary()
            ));
        }
        return;
    }
    if let Some(t) = d.state.task_mut(task) {
        t.state = TaskState::Reported;
        t.summary = Some(outcome.summary().to_owned());
    }
    let owner = t.owner.clone();
    let src = format!("agent:{owner}");
    match outcome {
        WorkerOutcome::Delivered { summary } | WorkerOutcome::Reviewed { summary, .. } => {
            d.context.push(ctx(
                ContextKind::Deliverable,
                ContextStatus::Claim,
                summary.clone(),
                Some(task),
                &src,
            ));
        }
        WorkerOutcome::Challenge { summary, details } => {
            let text = match details {
                Some(x) => format!("{summary} Evidence: {x}"),
                None => summary.clone(),
            };
            d.context.push(ctx(ContextKind::Disagreement, ContextStatus::Proposal, text, Some(task), &src));
        }
        WorkerOutcome::Blocker { summary } => {
            d.context.push(ctx(ContextKind::Risk, ContextStatus::Claim, summary.clone(), Some(task), &src));
        }
        _ => {}
    }
    if !evidence.changed_files.is_empty() {
        d.context.push(ctx(
            ContextKind::Progress,
            ContextStatus::Verified,
            format!("Files changed in the workspace so far: {}", evidence.changed_files.join(", ")),
            Some(task),
            "core",
        ));
    }
    if !evidence.outside_scope.is_empty() {
        let text = format!(
            "{} changed files outside its scope ({}): {}",
            title_of(&d.state, &owner),
            t.scope.join(", "),
            evidence.outside_scope.join(", ")
        );
        d.changes.push(Change::Observed { task: task.into(), text: text.clone() });
        d.context.push(ctx(ContextKind::Observation, ContextStatus::Verified, text, Some(task), "core"));
    }
    if let (Some(r), Some(parent)) = (d.state.reviewer_of(task), t.parent.clone()) {
        d.state.enqueue(
            &r,
            &parent,
            CheckpointItem::Reported {
                task: task.into(),
                outcome: outcome.clone(),
                evidence: evidence.clone(),
            },
        );
    }
}

/// Applies a supervisor's directive atomically: on any error nothing changes
/// (the caller works on a copy of the state).
fn apply_directive(
    d: &mut Decision,
    checkpoint: &Checkpoint,
    dir: &Directive,
    selections: &BTreeMap<String, Result<Selected, String>>,
    new_ids: &BTreeMap<String, Uuid>,
) -> Result<(), String> {
    let me = checkpoint.supervisor.clone();
    let before = (d.state.clone(), d.context.len(), d.changes.len(), d.closed_tasks.len());
    let r = apply_directive_inner(d, checkpoint, &me, dir, selections, new_ids);
    if r.is_err() {
        d.state = before.0;
        d.context.truncate(before.1);
        d.changes.truncate(before.2);
        d.closed_tasks.truncate(before.3);
        d.created_tasks.clear();
    }
    r
}

/// Keys a directive reuses, all at once and with what to do instead: a
/// supervisor that resends earlier, accepted parts of its plan would otherwise
/// learn of one repeat per rejected turn.
fn repeated_keys(state: &OrgState, dir: &Directive) -> Vec<String> {
    let mut out = Vec::new();
    let agents: Vec<&str> =
        dir.agents.iter().map(|a| a.key.as_str()).filter(|k| state.agent(k).is_some()).collect();
    if !agents.is_empty() {
        out.push(format!(
            "agent {} already exists: leave existing agents out of \"agents\" and name them as a task's owner",
            agents.join(", ")
        ));
    }
    for t in dir.tasks.iter().filter_map(|t| state.task(&t.key)) {
        out.push(format!(
            "a task with key {} already exists ({}): keys are never reused; to have that task redone, assess it \
             with verdict \"correct\"; for another round of work (a re-review after a fix, say), add a task with a \
             new key such as {}-2",
            t.key,
            t.state.as_str(),
            t.key
        ));
    }
    let new: BTreeSet<&str> = dir.tasks.iter().map(|t| t.key.as_str()).collect();
    for t in &dir.tasks {
        if let Some(c) = t.continues.as_deref()
            && state.task(c).is_none()
            && !new.contains(c)
        {
            out.push(format!(
                "task {}: \"continues\" names {c}, which is not a task key of the team (use a key such as {ROOT})",
                t.key
            ));
        }
    }
    out
}

fn apply_directive_inner(
    d: &mut Decision,
    checkpoint: &Checkpoint,
    me: &str,
    dir: &Directive,
    selections: &BTreeMap<String, Result<Selected, String>>,
    new_ids: &BTreeMap<String, Uuid>,
) -> Result<(), String> {
    let limits = d.state.limits.clone();
    let src = format!("agent:{me}");
    let repeated = repeated_keys(&d.state, dir);
    if !repeated.is_empty() {
        return Err(repeated.join("; "));
    }
    // Agents.
    for a in &dir.agents {
        if d.state.agent(&a.key).is_some() {
            return Err(format!("an agent with key {} already exists", a.key));
        }
        let boss = a.reports_to.clone().unwrap_or_else(|| me.to_owned());
        let Some(boss_agent) = d.state.agent(&boss).cloned() else {
            return Err(format!("{} would report to {boss}, who is not in the organization", a.key));
        };
        if !d.state.in_subtree(&boss, me) {
            return Err(format!("{} can only report to you or to someone in your scope", a.key));
        }
        if boss_agent.kind != AgentKind::Supervisor {
            return Err(format!("{} would report to {boss}, who is not a supervisor", a.key));
        }
        let depth = boss_agent.depth + 1;
        if depth > limits.max_depth {
            return Err(format!(
                "{} would be at level {depth}; the organization allows {} levels",
                a.key, limits.max_depth
            ));
        }
        if d.state.agents.len() as u32 >= limits.max_agents {
            return Err(format!(
                "the organization already has {} agents, its limit; finish with them or ask the user to raise the limit",
                d.state.agents.len()
            ));
        }
        let selected = match selections.get(&a.key) {
            Some(Ok(s)) => s.clone(),
            Some(Err(e)) => return Err(format!("no provider for {}: {e}", a.key)),
            None => return Err(format!("no provider was selected for {}", a.key)),
        };
        let id = *new_ids.get(&format!("agent:{}", a.key)).ok_or_else(|| format!("no id for {}", a.key))?;
        d.state.agents.push(Agent {
            id,
            key: a.key.clone(),
            title: a.title.trim().into(),
            kind: a.kind,
            reports_to: Some(boss),
            responsibility: a.responsibility.trim().into(),
            scope: a.scope.clone(),
            capabilities: a.capabilities.clone(),
            provider: selected.provider,
            model: selected.model,
            selection: selected.rationale,
            status: AgentStatus::Idle,
            created_by: me.into(),
            depth,
        });
        d.changes.push(Change::AgentAdded { key: a.key.clone() });
    }
    // Tasks.
    let mut planned: BTreeSet<String> = BTreeSet::new();
    let scope_agreed = d.state.scope_agreed;
    // The first breakdown of an agreed coordination task (the objective, or an
    // area delegated to a supervisor) is part of the agreed scope.
    let first_breakdown: BTreeSet<String> = d
        .state
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Planning && !t.late)
        .map(|t| t.key.clone())
        .collect();
    for t in &dir.tasks {
        if d.state.task(&t.key).is_some() {
            return Err(format!("a task with key {} already exists", t.key));
        }
        let parent = t.parent.clone().unwrap_or_else(|| checkpoint.task.clone());
        let Some(p) = d.state.task(&parent).cloned() else {
            return Err(format!("task {}: its parent {parent} does not exist", t.key));
        };
        if p.work != WorkKind::Coordination || p.owner != me || p.state.is_final() {
            return Err(format!(
                "task {}: you can only add work under an open coordination task you own",
                t.key
            ));
        }
        let Some(owner) = d.state.agent(&t.owner).cloned() else {
            return Err(format!("task {}: its owner {} is not in the organization", t.key, t.owner));
        };
        if !d.state.in_subtree(&owner.key, me) {
            return Err(format!("task {}: its owner must be someone in your scope", t.key));
        }
        let fits = match t.work {
            WorkKind::Coordination => owner.kind == AgentKind::Supervisor && owner.key != me,
            _ => owner.kind == AgentKind::Specialist,
        };
        if !fits {
            return Err(format!(
                "task {}: {} work must be owned by {}",
                t.key,
                t.work.as_str(),
                if t.work == WorkKind::Coordination {
                    "a supervisor that reports to you"
                } else {
                    "a specialist"
                }
            ));
        }
        if t.work.changes_files() && t.scope.is_empty() && owner.scope.is_empty() {
            return Err(format!("task {}: work that changes files needs a scope", t.key));
        }
        // A task this directive cancels can be replaced by one of the same
        // title in the same directive (assessments apply after new tasks).
        let title = norm_title(&t.title);
        let replaced = |k: &str| dir.assessments.iter().any(|a| a.task == k && a.verdict == Verdict::Cancel);
        if d.state
            .children(&parent)
            .any(|s| s.state != TaskState::Cancelled && !replaced(&s.key) && norm_title(&s.title) == title)
        {
            return Err(format!("a task titled {:?} already exists under {parent}", t.title));
        }
        for dep in &t.depends_on {
            if d.state.task(dep).is_none() && !planned.contains(dep) {
                return Err(format!("task {} depends on {dep}, which does not exist (yet)", t.key));
            }
        }
        if d.state.tasks.len() as u32 >= limits.max_tasks {
            return Err(format!(
                "the organization already has {} tasks, its limit; ask the user to raise it",
                d.state.tasks.len()
            ));
        }
        // Everything waits for its dependencies; coordination is planned once they are accepted.
        // After the first plan, new implementation work must continue an agreed
        // task; anything else is new scope and waits for the user (ADR 0046).
        // Investigation, documentation, testing and review support the objective
        // without widening what the product does; a late coordination area may be
        // added, but its own breakdown is not agreed.
        let continues_agreed = t.continues.as_deref().is_some_and(|k| {
            d.state.task(k).is_some_and(|x| !matches!(x.state, TaskState::Proposed | TaskState::Cancelled))
        });
        let breakdown = first_breakdown.contains(&parent);
        let unagreed = scope_agreed && !continues_agreed && !breakdown;
        let new_scope = unagreed && t.work == WorkKind::Implementation;
        let late = unagreed && t.work == WorkKind::Coordination;
        let state = if new_scope { TaskState::Proposed } else { TaskState::Waiting };
        let scope = if t.scope.is_empty() { owner.scope.clone() } else { t.scope.clone() };
        let task_id =
            *new_ids.get(&format!("task:{}", t.key)).ok_or_else(|| format!("no id for task {}", t.key))?;
        d.state.tasks.push(OrgTask {
            key: t.key.clone(),
            task_id: Some(task_id),
            title: t.title.trim().into(),
            description: t.description.clone(),
            work: t.work,
            parent: Some(parent.clone()),
            owner: owner.key.clone(),
            scope,
            acceptance: t.acceptance.clone(),
            depends_on: t.depends_on.clone(),
            state,
            corrections: 0,
            attempts: 0,
            turns: 0,
            late,
            instructions: Vec::new(),
            current_instructions: Vec::new(),
            summary: None,
            created_by: me.into(),
        });
        if let Some(p) = d.state.task_mut(&parent)
            && p.state == TaskState::Planning
        {
            p.state = TaskState::Delegated;
        }
        planned.insert(t.key.clone());
        d.created_tasks.push(t.key.clone());
        d.changes.push(Change::TaskDelegated { key: t.key.clone() });
        if new_scope {
            d.changes.push(Change::ScopeProposed { key: t.key.clone() });
            d.context.push(ctx(
                ContextKind::Decision,
                ContextStatus::Proposal,
                format!(
                    "{} proposes new work outside the agreed plan: \u{201c}{}\u{201d}. It starts only if the user approves it.",
                    title_of(&d.state, me),
                    t.title.trim()
                ),
                Some(&t.key),
                &format!("agent:{me}"),
            ));
        }
    }
    if !d.created_tasks.is_empty() {
        d.state.scope_agreed = true;
    }
    // Assessments.
    for a in &dir.assessments {
        let Some(t) = d.state.task(&a.task).cloned() else {
            return Err(format!("assessment of {}: no such task", a.task));
        };
        if d.state.reviewer_of(&a.task).as_deref() != Some(me) {
            return Err(format!("assessment of {}: it is not in your scope", a.task));
        }
        if t.state.is_final() {
            return Err(format!("assessment of {}: it is already {}", a.task, t.state.as_str()));
        }
        let evidence = a.evidence.as_deref().map(|e| format!(" Evidence: {e}")).unwrap_or_default();
        match a.verdict {
            Verdict::Accept => {
                if t.state != TaskState::Reported {
                    return Err(format!("assessment of {}: only reported work can be accepted", a.task));
                }
                let tt = d.state.task_mut(&a.task).ok_or("missing task")?;
                tt.state = TaskState::Accepted;
                d.state.turns_since_progress = 0;
                d.state.goal_due = true;
                d.closed_tasks.push((a.task.clone(), true));
                if t.work == WorkKind::Coordination
                    && let Some(o) = d.state.agent_mut(&t.owner)
                {
                    o.status = AgentStatus::Done;
                }
                d.context.push(ctx(
                    ContextKind::Deliverable,
                    ContextStatus::Decided,
                    format!(
                        "{} accepted {}: {}.{evidence}",
                        title_of(&d.state, me),
                        t.title,
                        t.summary.as_deref().unwrap_or_default().trim().trim_end_matches('.')
                    ),
                    Some(&a.task),
                    &src,
                ));
            }
            Verdict::Correct => {
                if t.state != TaskState::Reported {
                    return Err(format!("assessment of {}: only reported work can be corrected", a.task));
                }
                let text = a.instructions.clone().unwrap_or_default();
                if t.corrections >= d.state.limits.max_corrections {
                    return Err(format!(
                        "{} has had {} correction rounds, the limit: accept, reassign or cancel it, or escalate to the user",
                        a.task, t.corrections
                    ));
                }
                let tt = d.state.task_mut(&a.task).ok_or("missing task")?;
                tt.corrections += 1;
                tt.attempts = 0;
                if t.work == WorkKind::Coordination {
                    // The sub-supervisor gets the instruction in its next supervision turn.
                    tt.state = TaskState::Delegated;
                    d.state.enqueue(
                        &t.owner,
                        &a.task,
                        CheckpointItem::Instruction { from: me.into(), text: text.clone() },
                    );
                } else {
                    tt.state = TaskState::Ready;
                    tt.instructions.push(text.clone());
                }
                d.context.push(ctx(
                    ContextKind::Instruction,
                    ContextStatus::Decided,
                    format!("{text}{evidence}"),
                    Some(&a.task),
                    &src,
                ));
            }
            Verdict::Reassign => {
                let to = a.to.clone().unwrap_or_default();
                let Some(new_owner) = d.state.agent(&to).cloned() else {
                    return Err(format!("reassignment of {}: {to} is not in the organization", a.task));
                };
                if !d.state.in_subtree(&to, me)
                    || new_owner.kind != AgentKind::Specialist
                    || t.work == WorkKind::Coordination
                {
                    return Err(format!(
                        "reassignment of {}: {to} must be a specialist in your scope",
                        a.task
                    ));
                }
                if matches!(t.state, TaskState::Working) {
                    return Err(format!("reassignment of {}: it is being worked on", a.task));
                }
                let tt = d.state.task_mut(&a.task).ok_or("missing task")?;
                tt.owner = to.clone();
                tt.attempts = 0;
                tt.state = if t.state == TaskState::Reported { TaskState::Ready } else { t.state };
                if let Some(i) = &a.instructions {
                    tt.instructions.push(i.clone());
                }
                d.context.push(ctx(
                    ContextKind::Decision,
                    ContextStatus::Decided,
                    format!(
                        "{} reassigned {} from {} to {}.{evidence}",
                        title_of(&d.state, me),
                        t.title,
                        title_of(&d.state, &t.owner),
                        title_of(&d.state, &to)
                    ),
                    Some(&a.task),
                    &src,
                ));
            }
            Verdict::Cancel => {
                if matches!(t.state, TaskState::Working) {
                    return Err(format!(
                        "{} is being worked on; it can be cancelled when it reports",
                        a.task
                    ));
                }
                let reason = a.instructions.clone().unwrap_or_default();
                // Cancelling a coordination task cancels what is open under it.
                let mut stack = vec![a.task.clone()];
                while let Some(k) = stack.pop() {
                    let kids: Vec<String> = d.state.children(&k).map(|c| c.key.clone()).collect();
                    if let Some(tt) = d.state.task_mut(&k)
                        && !tt.state.is_final()
                    {
                        if tt.state == TaskState::Working {
                            return Err(format!(
                                "{k} is being worked on; it can be cancelled when it reports"
                            ));
                        }
                        tt.state = TaskState::Cancelled;
                        d.closed_tasks.push((k.clone(), false));
                    }
                    stack.extend(kids);
                }
                d.context.push(ctx(
                    ContextKind::Decision,
                    ContextStatus::Decided,
                    format!("Cancelled {}: {reason}", t.title),
                    Some(&a.task),
                    &src,
                ));
            }
        }
        d.changes.push(Change::Assessed {
            task: a.task.clone(),
            verdict: a.verdict,
            by: me.into(),
            text: a.instructions.clone(),
        });
    }
    // Context notes.
    for n in &dir.context {
        if let Some(k) = &n.task
            && d.state.task(k).is_none()
        {
            return Err(format!("context note about {k}: no such task"));
        }
        d.context.push(NewContext {
            kind: n.kind,
            status: n.status.unwrap_or(ContextStatus::Claim),
            text: n.text.trim().into(),
            task: n.task.clone(),
            agent: None,
            source: src.clone(),
            supersedes: n.supersedes,
        });
    }
    d.context.push(ctx(
        ContextKind::Progress,
        ContextStatus::Claim,
        dir.summary.trim().into(),
        Some(&checkpoint.task),
        &src,
    ));
    // Escalation.
    if let Some(e) = &dir.escalate {
        let boss = d.state.agent(me).and_then(|a| a.reports_to.clone());
        d.context.push(ctx(
            ContextKind::Escalation,
            ContextStatus::Claim,
            e.question.clone(),
            Some(&checkpoint.task),
            &src,
        ));
        match (e.to, boss) {
            (EscalateTo::Supervisor, Some(boss)) => {
                let home = d.state.home_task(&boss).unwrap_or_else(|| ROOT.into());
                d.state.enqueue(
                    &boss,
                    &home,
                    CheckpointItem::Escalated { from: me.into(), question: e.question.clone() },
                );
                d.changes.push(Change::Escalated {
                    by: me.into(),
                    to: EscalateTo::Supervisor,
                    question: e.question.clone(),
                });
            }
            _ => {
                d.state.question_from = Some(me.into());
                d.changes.push(Change::Escalated {
                    by: me.into(),
                    to: EscalateTo::User,
                    question: e.question.clone(),
                });
                wait(d, format!("{} asks you: {}", title_of(&d.state, me), e.question));
            }
        }
    }
    // Done.
    if dir.done {
        let home = checkpoint.task.clone();
        let open: Vec<String> =
            d.state.children(&home).filter(|c| !c.state.is_final()).map(|c| c.title.clone()).collect();
        if !open.is_empty() {
            return Err(format!(
                "your scope is not finished: {} still open (accept or cancel them first)",
                open.join(", ")
            ));
        }
        if d.state.children(&home).next().is_none() && !dir.tasks.is_empty() {
            return Err("done with new tasks in the same directive".into());
        }
        let summary = dir.summary.trim().to_owned();
        let tt = d.state.task_mut(&home).ok_or("missing task")?;
        tt.summary = Some(summary.clone());
        let parent = tt.parent.clone();
        match parent {
            None => {
                tt.state = TaskState::Accepted;
                d.closed_tasks.push((home.clone(), true));
                if let Some(a) = d.state.agent_mut(me) {
                    a.status = AgentStatus::Done;
                }
                d.state.status = OrgStatus::Done;
                d.state.final_summary = Some(summary.clone());
                d.state.reason = Some(summary.clone());
                d.changes.push(Change::Done { summary: summary.clone() });
                d.action = Action::Deliver { summary };
            }
            Some(p) => {
                // A sub-supervisor reports its finished scope; its own supervisor reviews it.
                tt.state = TaskState::Reported;
                if let Some(r) = d.state.reviewer_of(&home) {
                    let reported = CheckpointItem::Reported {
                        task: home.clone(),
                        outcome: WorkerOutcome::Delivered { summary },
                        evidence: Evidence::default(),
                    };
                    d.state.enqueue(&r, &p, reported);
                }
            }
        }
    } else if dir.tasks.is_empty() && dir.escalate.is_none() {
        let home = &checkpoint.task;
        let open_home =
            d.state.task(home).is_some_and(|t| !t.state.is_final() && t.state != TaskState::Reported);
        let kids: Vec<&OrgTask> = d.state.children(home).collect();
        let planning = checkpoint.items.contains(&CheckpointItem::Plan);
        let told_finished = checkpoint.items.contains(&CheckpointItem::ChildrenFinished);
        if planning && open_home && kids.is_empty() {
            return Err("a plan needs at least one task (or \"done\": true if nothing is needed)".into());
        }
        let busy =
            kids.iter().any(|k| !k.state.is_final()) || d.state.queue.iter().any(|c| c.supervisor == me);
        if told_finished && open_home && !busy {
            return Err(
                "every task in your scope is accepted or cancelled: add tasks, or say \"done\": true \
                        with a summary that names any outstanding issues"
                    .into(),
            );
        }
    }
    Ok(())
}

/// Starts whatever can run next, if nothing runs.
/// A review looks at finished work: it waits while other work in its area is
/// still open (a fix the lead added after the first review, say), unless that
/// work waits for the review. Work only the user can start (proposed) does not
/// hold it back.
fn review_waits(state: &OrgState, t: &OrgTask) -> bool {
    t.work == WorkKind::Review
        && t.parent.as_deref().is_some_and(|p| {
            state.children(p).any(|s| {
                s.key != t.key
                    && s.work != WorkKind::Review
                    && !s.state.is_final()
                    && s.state != TaskState::Proposed
                    && !s.depends_on.contains(&t.key)
            })
        })
}

fn schedule(d: &mut Decision, now_ms: i64) {
    if d.state.status != OrgStatus::Running || d.state.activity != Activity::Idle {
        return;
    }
    let l = d.state.limits.clone();
    let minutes = (now_ms - d.state.started_at_ms).max(0) / 60_000;
    if minutes >= i64::from(l.max_minutes) {
        return wait(
            d,
            format!(
                "the organization reached its time limit ({} minutes). Resume to continue for another {} minutes.",
                l.max_minutes, l.max_minutes
            ),
        );
    }
    if l.max_cost_usd.is_some_and(|m| d.state.cost_usd >= m) {
        return wait(
            d,
            format!(
                "the organization reached its cost limit (${:.2}). Raise it in the project settings to continue.",
                d.state.cost_usd
            ),
        );
    }
    if l.max_tokens.is_some_and(|m| d.state.tokens >= m) {
        return wait(
            d,
            format!(
                "the organization reached its token limit ({}). Raise it in the project settings to continue.",
                d.state.tokens
            ),
        );
    }
    // Tasks whose dependencies are accepted become ready (coordination: ready to plan).
    let accepted: BTreeSet<String> =
        d.state.tasks.iter().filter(|t| t.state == TaskState::Accepted).map(|t| t.key.clone()).collect();
    for t in d.state.tasks.iter_mut() {
        if t.state == TaskState::Waiting && t.depends_on.iter().all(|k| accepted.contains(k)) {
            t.state = if t.work == WorkKind::Coordination { TaskState::Planning } else { TaskState::Ready };
        }
    }
    // Planning and finished coordination tasks become checkpoints.
    let planning: Vec<(String, String)> = d
        .state
        .tasks
        .iter()
        .filter(|t| t.work == WorkKind::Coordination && t.state == TaskState::Planning)
        .map(|t| (t.owner.clone(), t.key.clone()))
        .collect();
    for (owner, key) in planning {
        let queued = d.state.queue.iter().any(|c| c.task == key && c.items.contains(&CheckpointItem::Plan));
        if !queued {
            d.state.enqueue(&owner, &key, CheckpointItem::Plan);
        }
    }
    let finished: Vec<(String, String)> = d
        .state
        .tasks
        .iter()
        .filter(|t| t.work == WorkKind::Coordination && t.state == TaskState::Delegated)
        .filter(|t| {
            let mut kids = d.state.children(&t.key).peekable();
            kids.peek().is_some() && kids.all(|c| c.state.is_final())
        })
        .map(|t| (t.owner.clone(), t.key.clone()))
        .collect();
    for (owner, key) in finished {
        let queued = d
            .state
            .queue
            .iter()
            .any(|c| c.task == key && c.items.contains(&CheckpointItem::ChildrenFinished));
        if !queued {
            d.state.enqueue(&owner, &key, CheckpointItem::ChildrenFinished);
        }
    }
    if !d.state.queue.is_empty() {
        if d.state.supervision_turns >= l.max_supervision_turns {
            return wait(
                d,
                format!(
                    "the organization used its {} supervision turns. Raise the limit in the project settings, then resume.",
                    l.max_supervision_turns
                ),
            );
        }
        if d.state.turns_since_progress >= l.max_turns_without_progress {
            return wait(
                d,
                format!(
                    "{} supervision turns in a row accepted no work: the team may be going in circles. Read the \
                     decisions, then resume to let it continue, give the lead a direction, or stop the team.",
                    d.state.turns_since_progress
                ),
            );
        }
        let checkpoint = d.state.queue.remove(0);
        d.state.supervision_turns += 1;
        d.state.turns_since_progress += 1;
        if let Some(a) = d.state.agent_mut(&checkpoint.supervisor) {
            a.status = AgentStatus::Active;
        }
        d.state.activity = Activity::Supervising { checkpoint: checkpoint.clone() };
        d.action = Action::Supervise { checkpoint };
        return;
    }
    let next = d
        .state
        .tasks
        .iter()
        .find(|t| t.state == TaskState::Ready && t.task_id.is_some() && !review_waits(&d.state, t))
        .map(|t| (t.key.clone(), t.owner.clone()));
    if let Some((key, owner)) = next {
        if let Some(t) = d.state.task(&key)
            && t.turns >= l.max_task_turns
        {
            let last =
                t.summary.clone().map(|s| format!(" The last turn ended with: {s}")).unwrap_or_default();
            return wait(
                d,
                format!(
                    "\u{201c}{}\u{201d} was worked on {} times without being accepted: it may be going in circles.{last} \
                     Resume to allow one more attempt, give the lead a direction, or stop the team.",
                    t.title, t.turns
                ),
            );
        }
        let instructions = match d.state.task_mut(&key) {
            Some(t) => {
                t.state = TaskState::Working;
                t.turns += 1;
                t.current_instructions = std::mem::take(&mut t.instructions);
                t.current_instructions.clone()
            }
            None => Vec::new(),
        };
        if let Some(a) = d.state.agent_mut(&owner) {
            a.status = AgentStatus::Active;
        }
        d.state.activity = Activity::Working { task: key.clone(), agent: owner.clone(), interrupting: false };
        d.action = Action::Work { task: key, agent: owner, instructions };
        return;
    }
    // A waiting task that depends on cancelled work can never start: its
    // supervisor decides (cancel it or re-plan it), once per task.
    let dead: Vec<(String, String, Vec<String>)> = d
        .state
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Waiting && !d.state.dead_ends_reported.contains(&t.key))
        .filter_map(|t| {
            let cancelled: Vec<String> = t
                .depends_on
                .iter()
                .filter(|k| d.state.task(k).is_some_and(|x| x.state == TaskState::Cancelled))
                .cloned()
                .collect();
            (!cancelled.is_empty()).then(|| (t.key.clone(), t.title.clone(), cancelled))
        })
        .collect();
    if !dead.is_empty() {
        for (key, title, cancelled) in dead {
            d.state.dead_ends_reported.push(key.clone());
            let names: Vec<String> =
                cancelled.iter().map(|k| d.state.task(k).map_or(k.clone(), |t| t.title.clone())).collect();
            let text = format!(
                "\u{201c}{title}\u{201d} waits for cancelled work ({}), so it can never start: cancel it, or plan it again without that work.",
                names.join(", ")
            );
            d.context.push(ctx(
                ContextKind::Observation,
                ContextStatus::Verified,
                text.clone(),
                Some(&key),
                "core",
            ));
            if let Some(r) = d.state.reviewer_of(&key) {
                let home = d.state.task(&key).and_then(|t| t.parent.clone()).unwrap_or_else(|| ROOT.into());
                d.state.enqueue(&r, &home, CheckpointItem::Observed { task: key.clone(), text });
            }
        }
        if !d.state.queue.is_empty() {
            return schedule(d, now_ms);
        }
    }
    // Nothing can run, and the objective is not finished: say why.
    let root_done = d.state.task(ROOT).is_some_and(|t| t.state.is_final());
    let proposed: Vec<String> = d
        .state
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Proposed)
        .map(|t| format!("\u{201c}{}\u{201d}", t.title))
        .collect();
    if !root_done && !proposed.is_empty() {
        return wait(
            d,
            format!(
                "The lead proposes new work outside the agreed plan: {}. Approve or decline it under Needs you.",
                proposed.join(", ")
            ),
        );
    }
    if !root_done {
        let stuck: Vec<String> = d
            .state
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Waiting)
            .map(|t| {
                let missing: Vec<&str> =
                    t.depends_on.iter().filter(|k| !accepted.contains(*k)).map(String::as_str).collect();
                format!("{} waits for {}", t.title, missing.join(", "))
            })
            .collect();
        let why = if stuck.is_empty() {
            "no work is ready and no supervisor has anything to review".to_owned()
        } else {
            stuck.join("; ")
        };
        wait(
            d,
            format!(
                "Nothing can run: {why}. Tell the lead how to continue (Ask the supervisor, or answer here), then resume; or stop the team."
            ),
        );
    }
}

/// Whether a goal check is due: work was accepted since the last one, or the
/// team has worked `interval_ms` since the last one (or since it started).
pub fn goal_check_due(state: &OrgState, now_ms: i64, interval_ms: i64) -> bool {
    if state.status != OrgStatus::Running || state.tasks.iter().all(|t| t.key == ROOT) {
        return false;
    }
    state.goal_due || now_ms - state.last_goal_check_ms.max(state.started_at_ms) >= interval_ms
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
pub(crate) use tests as tests_support;
