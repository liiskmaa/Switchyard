use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

string_enum!(
    /// What a workspace record describes.
    WorkspaceKind {
        /// The project's own checkout, recorded by milestone 2 before
        /// workspace isolation existed. Kept only so old sessions stay
        /// attached to history; it never accepts new sessions and is never
        /// cleaned up.
        LegacyCheckout => "legacy_checkout",
        /// A Core-owned Git worktree on its own `switchyard/<id>` branch.
        Worktree => "worktree",
    }
);

string_enum!(
    /// Persisted workspace lifecycle. See [`WorkspaceStatus::can_transition`].
    WorkspaceStatus {
        /// The record exists; the worktree is being created and verified.
        Provisioning => "provisioning",
        /// Verified, no session attached, no uncommitted changes.
        Ready => "ready",
        /// A session is attached (one at most).
        Active => "active",
        /// No session attached; the worktree has uncommitted or untracked
        /// changes.
        Dirty => "dirty",
        /// Reserved for a later milestone (integration back into the
        /// project). Nothing transitions into it yet.
        Integrating => "integrating",
        /// The operator marked the work in this workspace as done. It can
        /// still be reopened by starting a session.
        Completed => "completed",
        /// Provisioning failed, or the worktree was found missing, invalid or
        /// on an unexpected branch. `status_reason` says which. Recovered
        /// only by an explicit verify.
        Failed => "failed",
        /// Cleanup was confirmed and is in progress.
        CleanupPending => "cleanup_pending",
        /// Kept on disk but hidden from normal work; no sessions.
        Archived => "archived",
        /// The worktree directory was removed by an explicit cleanup. The
        /// record and the branch are kept.
        Removed => "removed",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal workspace status transition {from} → {to}")]
pub struct TransitionError {
    pub from: WorkspaceStatus,
    pub to: WorkspaceStatus,
}

impl WorkspaceStatus {
    /// Legal transitions. Self-transitions are allowed (they update fields
    /// without changing the status).
    pub fn can_transition(self, to: Self) -> bool {
        use WorkspaceStatus::*;
        if self == to {
            return !matches!(self, Removed);
        }
        matches!(
            (self, to),
            (Provisioning, Ready | Dirty | Failed)
                | (Ready, Dirty | Active | Completed | Archived | Failed)
                | (Dirty, Ready | Active | Completed | Archived | Failed)
                | (Completed, Ready | Dirty | Active | Archived | Failed)
                | (Active, Ready | Dirty | Failed)
                | (Failed, Ready | Dirty | Archived)
                | (Archived, Ready | Dirty | CleanupPending | Failed)
                | (CleanupPending, Archived | Removed)
        )
    }

    pub fn transition(self, to: Self) -> Result<Self, TransitionError> {
        if self.can_transition(to) { Ok(to) } else { Err(TransitionError { from: self, to }) }
    }

    /// A new or resumed session may attach.
    pub fn accepts_session(self) -> bool {
        matches!(self, Self::Ready | Self::Dirty | Self::Completed)
    }

    /// Settled, no session attached: the state follows the worktree's Git
    /// status (`ready` ↔ `dirty`).
    pub fn tracks_git_status(self) -> bool {
        matches!(self, Self::Ready | Self::Dirty)
    }

    /// The worktree directory is expected to exist.
    pub fn expects_worktree(self) -> bool {
        matches!(self, Self::Ready | Self::Dirty | Self::Active | Self::Completed | Self::Integrating)
    }

    /// Requires `archived_at` to be set.
    pub fn is_archived(self) -> bool {
        matches!(self, Self::Archived | Self::CleanupPending | Self::Removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_round_trip() {
        for s in WorkspaceStatus::ALL {
            assert_eq!(s.as_str().parse::<WorkspaceStatus>(), Ok(*s));
            assert_eq!(serde_json::to_value(s).unwrap_or_default(), s.as_str());
        }
        for k in WorkspaceKind::ALL {
            assert_eq!(k.as_str().parse::<WorkspaceKind>(), Ok(*k));
        }
        assert!("nope".parse::<WorkspaceStatus>().is_err());
    }

    #[test]
    fn lifecycle_rules() {
        use WorkspaceStatus::*;
        // The typical path.
        for (a, b) in [(Provisioning, Ready), (Ready, Active), (Active, Dirty), (Dirty, Archived)] {
            assert!(a.can_transition(b), "{a} → {b}");
        }
        // A session's end never removes a workspace; only an explicit
        // cleanup from `archived` can.
        for s in WorkspaceStatus::ALL {
            assert_eq!(s.can_transition(Removed), *s == CleanupPending, "{s} → removed");
            assert_eq!(
                s.can_transition(CleanupPending),
                matches!(s, Archived | CleanupPending),
                "{s} → cleanup_pending"
            );
        }
        // An attached workspace cannot be archived or cleaned up.
        assert!(!Active.can_transition(Archived));
        // Two sessions can never share a workspace.
        assert!(!Active.can_transition(Active) || !Active.accepts_session());
        assert!(!Active.accepts_session());
        // Integrating is reserved: unreachable for now.
        assert!(WorkspaceStatus::ALL.iter().all(|s| *s == Integrating || !s.can_transition(Integrating)));
        // Removed is final.
        assert!(WorkspaceStatus::ALL.iter().all(|s| !Removed.can_transition(*s)));
        assert!(Provisioning.transition(Active).is_err());
    }
}
