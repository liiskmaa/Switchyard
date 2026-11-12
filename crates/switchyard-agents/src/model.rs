use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

string_enum!(
    /// A native agent vendor. Stored as `agents.vendor` / `sessions.provider`.
    AgentProvider { ClaudeCode => "claude_code", Codex => "codex" }
);

string_enum!(
    /// Lifecycle of one Switchyard agent session.
    ///
    /// `Starting` → process spawned; `Initializing` → waiting for the agent's
    /// first handshake; `Idle` → ready for input; `Running` → a turn is in
    /// progress; `WaitingForApproval` → the turn is blocked on a permission
    /// decision; `Interrupted` → the last turn was interrupted, the process is
    /// alive and accepts input. `Completed`, `Failed` and `Stopped` are
    /// terminal: no process is attached.
    SessionState {
        Starting => "starting",
        Initializing => "initializing",
        Idle => "idle",
        Running => "running",
        WaitingForApproval => "waiting_for_approval",
        Interrupted => "interrupted",
        Completed => "completed",
        Failed => "failed",
        Stopped => "stopped",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal session state transition {from} → {to}")]
pub struct StateError {
    pub from: SessionState,
    pub to: SessionState,
}

impl SessionState {
    /// No process is attached; only an explicit resume can continue it.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }

    /// A process is (or is about to be) attached.
    pub fn is_live(self) -> bool {
        !self.is_terminal()
    }

    /// The agent can accept a new user message as a new turn right now.
    pub fn accepts_new_turn(self) -> bool {
        matches!(self, Self::Idle | Self::Interrupted)
    }

    /// A turn is in progress (it can be interrupted).
    pub fn turn_in_progress(self) -> bool {
        matches!(self, Self::Running | Self::WaitingForApproval)
    }

    /// Validates a transition. Any live state may fail or stop; the rest of
    /// the graph follows the documented lifecycle.
    pub fn transition(self, to: Self) -> Result<Self, StateError> {
        use SessionState::*;
        let ok = match (self, to) {
            (a, b) if a == b => true,
            (a, Failed | Stopped | Completed) if a.is_live() => true,
            (Starting, Initializing | Idle | Running) => true,
            (Initializing, Idle | Running) => true,
            (Idle | Interrupted, Running) => true,
            (Running, Idle | WaitingForApproval | Interrupted) => true,
            (WaitingForApproval, Running | Interrupted | Idle) => true,
            // Explicit resume of a finished session starts a new process.
            (Completed | Failed | Stopped, Starting) => true,
            _ => false,
        };
        if ok { Ok(to) } else { Err(StateError { from: self, to }) }
    }
}

string_enum!(
    /// Status of a centralized approval.
    ApprovalStatus {
        Pending => "pending",
        Approved => "approved",
        Denied => "denied",
        Expired => "expired",
        Cancelled => "cancelled",
    }
);

impl ApprovalStatus {
    pub fn is_final(self) -> bool {
        self != Self::Pending
    }
}

string_enum!(
    /// Who or what settled an approval.
    DecisionSource {
        Operator => "operator",
        Timeout => "timeout",
        AgentCancelled => "agent_cancelled",
        AgentExited => "agent_exited",
        CoreShutdown => "core_shutdown",
        CoreRestart => "core_restart",
        Policy => "policy",
    }
);

/// The decision delivered to the agent for one permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Run the tool with its original input.
    Allow,
    /// Do not run the tool; the message is shown to the agent.
    Deny { message: String },
}

/// What an adapter can do. Core and the UI must consult this instead of
/// assuming every agent supports every operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Several turns can be driven through one process.
    pub multi_turn: bool,
    /// Incremental assistant text is streamed.
    pub streaming_text: bool,
    /// A running turn can be interrupted while the process stays alive.
    pub interrupt_turn: bool,
    /// A message sent during a running turn is queued and starts the next
    /// turn (it does not alter the running turn).
    pub queue_message_during_turn: bool,
    /// New guidance can be injected into the *running* turn.
    pub steer_running_turn: bool,
    /// A finished session can be continued in a new process.
    pub resume_session: bool,
    /// Tool permission requests are routed to Switchyard.
    pub permission_requests: bool,
    /// Switchyard can choose the native session id before launch.
    pub caller_assigned_session_id: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_roundtrip() {
        for s in SessionState::ALL {
            assert_eq!(s.as_str().parse::<SessionState>(), Ok(*s));
        }
        assert!("bogus".parse::<ApprovalStatus>().is_err());
        assert_eq!(
            serde_json::to_string(&SessionState::WaitingForApproval).ok().as_deref(),
            Some("\"waiting_for_approval\"")
        );
    }

    #[test]
    fn lifecycle_transitions() {
        use SessionState::*;
        let path = [
            Starting,
            Initializing,
            Idle,
            Running,
            WaitingForApproval,
            Running,
            Idle,
            Running,
            Interrupted,
            Running,
            Idle,
            Stopped,
            Starting,
        ];
        let mut state = path[0];
        for next in &path[1..] {
            state = state.transition(*next).unwrap();
        }
        assert!(Idle.transition(WaitingForApproval).is_err());
        assert!(Stopped.transition(Running).is_err());
        assert!(Completed.transition(Idle).is_err());
        assert!(Running.transition(Failed).is_ok());
        assert!(Failed.transition(Failed).is_ok());
    }

    #[test]
    fn state_predicates() {
        assert!(SessionState::Interrupted.accepts_new_turn());
        assert!(!SessionState::Running.accepts_new_turn());
        assert!(SessionState::WaitingForApproval.turn_in_progress());
        assert!(SessionState::Stopped.is_terminal());
        assert!(ApprovalStatus::Denied.is_final() && !ApprovalStatus::Pending.is_final());
    }
}
