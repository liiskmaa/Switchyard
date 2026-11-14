//! Task orchestration vocabulary and the task state machine.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl FromStr for $name {
            type Err = UnknownValue;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s { $($s => Ok(Self::$variant),)+ other => Err(UnknownValue {
                    kind: stringify!($name), value: other.to_owned() }) }
            }
        }
    };
}

string_enum!(
    /// Lifecycle of a task. A task is independent of any agent session; one
    /// task may involve several sessions over time (ADR 0023).
    TaskStatus {
        /// Being written by the operator; not eligible for work.
        Draft => "draft",
        /// Every dependency is completed; an agent may be assigned.
        Ready => "ready",
        /// Waiting for dependencies, or a dependency failed or was
        /// cancelled, or an agent reported a blocker.
        Blocked => "blocked",
        /// An assignment exists; its session has not started work yet.
        Assigned => "assigned",
        /// An investigating or implementing session is working on it.
        InProgress => "in_progress",
        /// Work reported (or ended); the operator decides what happens next.
        AwaitingReview => "awaiting_review",
        /// A review session is examining the work.
        InReview => "in_review",
        /// The operator accepted the result. Terminal.
        Completed => "completed",
        /// Work failed or was rejected; the operator may retry.
        Failed => "failed",
        /// Abandoned by the operator. Terminal.
        Cancelled => "cancelled",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal task transition {from} → {to}")]
pub struct TransitionError {
    pub from: TaskStatus,
    pub to: TaskStatus,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    /// A session is (or is about to be) working on the task.
    pub fn has_active_work(self) -> bool {
        matches!(self, Self::Assigned | Self::InProgress | Self::InReview)
    }

    /// Whether a task in this state counts as a satisfied dependency.
    pub fn satisfies_dependents(self) -> bool {
        self == Self::Completed
    }

    /// The legal transitions. Who may request each one (operator or Core) is
    /// enforced by Core; this only validates the graph.
    pub fn transition(self, to: Self) -> Result<Self, TransitionError> {
        use TaskStatus::*;
        let ok = match (self, to) {
            (a, b) if a == b => false,
            (a, Cancelled) if !a.is_terminal() => true,
            (Draft, Ready | Blocked) => true,
            (Ready, Assigned | Blocked) => true,
            (Blocked, Ready | Failed) => true,
            // Back to Ready / AwaitingReview when an assignment is cancelled or cannot start.
            (Assigned, InProgress | Ready | Blocked | InReview | AwaitingReview) => true,
            (InProgress, AwaitingReview | Blocked | Failed | Ready) => true,
            (AwaitingReview, Assigned | InProgress | InReview | Completed | Failed) => true,
            (InReview, AwaitingReview | Failed) => true,
            (Failed, Ready | Blocked) => true,
            _ => false,
        };
        if ok { Ok(to) } else { Err(TransitionError { from: self, to }) }
    }
}

string_enum!(
    /// What kind of work a task is.
    TaskKind {
        Investigation => "investigation",
        Implementation => "implementation",
        General => "general",
    }
);

string_enum!(
    Priority {
        Low => "low",
        Normal => "normal",
        High => "high",
        Urgent => "urgent",
    }
);

string_enum!(
    /// The part an agent plays in one assignment.
    Role {
        /// Investigate and report findings or a plan. Writes allowed (policy permitting).
        Investigate => "investigate",
        /// Change the workspace. Writes allowed (policy permitting).
        Implement => "implement",
        /// Examine the work. The session cannot change files (ADR 0027).
        Review => "review",
    }
);

impl Role {
    /// Review sessions never change files.
    pub fn writes(self) -> bool {
        self != Self::Review
    }
}

string_enum!(
    /// One agent's piece of work on a task (one or more turns of one session).
    AssignmentStatus {
        /// Created; its session has not been started yet.
        Pending => "pending",
        /// Its session is working.
        Active => "active",
        /// The agent reported; the assignment is finished.
        Reported => "reported",
        /// The session ended without a report, or could not start.
        Failed => "failed",
        /// Withdrawn by the operator.
        Cancelled => "cancelled",
    }
);

impl AssignmentStatus {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Active)
    }
}

string_enum!(
    /// Structured messages recorded by Core (ADR 0025).
    MessageKind {
        /// Core gave an agent its task context.
        TaskAssignment => "task_assignment",
        /// An agent's progress note.
        StatusUpdate => "status_update",
        /// An agent asks the operator a question.
        InfoRequest => "info_request",
        /// An agent's investigation findings.
        Findings => "findings",
        /// An agent proposes a plan or subtasks.
        Proposal => "proposal",
        /// A review session's verdict.
        ReviewResult => "review_result",
        /// An agent reports it cannot continue.
        Blocker => "blocker",
        /// An agent asks for work to be delegated (proposed subtasks).
        DelegationRequest => "delegation_request",
        /// An implementing agent reports its work.
        CompletionReport => "completion_report",
        /// The operator's note for the next turn of an agent.
        OperatorNote => "operator_note",
        /// The agent's turn ended without a valid report.
        MalformedReport => "malformed_report",
    }
);

impl MessageKind {
    /// Messages whose sender is an agent. They are claims or requests, never
    /// authorizations: Core records them, the operator decides.
    pub fn from_agent(self) -> bool {
        !matches!(self, Self::TaskAssignment | Self::OperatorNote)
    }
}

string_enum!(
    ReviewResult {
        Approved => "approved",
        ChangesRequested => "changes_requested",
        Rejected => "rejected",
        Blocked => "blocked",
    }
);

string_enum!(
    Severity {
        Info => "info",
        Low => "low",
        Medium => "medium",
        High => "high",
        Critical => "critical",
    }
);

string_enum!(
    /// An agent's proposed subtask, awaiting the operator (ADR 0026).
    ProposalStatus {
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use TaskStatus::*;

    #[test]
    fn strings_roundtrip() {
        for s in TaskStatus::ALL {
            assert_eq!(s.as_str().parse::<TaskStatus>(), Ok(*s));
        }
        assert!("done".parse::<TaskStatus>().is_err());
    }

    #[test]
    fn the_documented_workflow_is_legal() {
        let path = [
            Draft,
            Blocked,
            Ready,
            Assigned,
            InProgress,
            AwaitingReview,
            InReview,
            AwaitingReview,
            InProgress,
            AwaitingReview,
            Completed,
        ];
        let mut s = path[0];
        for next in &path[1..] {
            s = s.transition(*next).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn illegal_transitions_are_refused() {
        for (from, to) in [
            (Draft, InProgress),
            (Draft, Completed),
            (Ready, Completed),
            (InProgress, Completed),
            (InReview, Completed),
            (Completed, Ready),
            (Cancelled, Ready),
            (Completed, Cancelled),
            (Failed, Completed),
            (Ready, Ready),
        ] {
            assert!(from.transition(to).is_err(), "{from} → {to}");
        }
    }

    #[test]
    fn every_open_state_can_be_cancelled_and_failed_work_retried() {
        for s in TaskStatus::ALL.iter().filter(|s| !s.is_terminal()) {
            assert!(s.transition(Cancelled).is_ok(), "{s}");
        }
        assert!(Failed.transition(Ready).is_ok());
        assert!(Completed.is_terminal() && Cancelled.is_terminal() && !Failed.is_terminal());
    }

    #[test]
    fn only_completed_satisfies_dependents_and_reviews_never_write() {
        assert!(Completed.satisfies_dependents());
        assert!(!AwaitingReview.satisfies_dependents());
        assert!(!Role::Review.writes() && Role::Implement.writes());
        assert!(!MessageKind::OperatorNote.from_agent() && MessageKind::Proposal.from_agent());
    }
}
