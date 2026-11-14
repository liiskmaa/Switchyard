use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result { f.write_str(self.as_str()) }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::model::UnknownValue;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s { $($s => Ok(Self::$variant),)+ other => Err($crate::model::UnknownValue {
                    kind: stringify!($name), value: other.to_owned() }) }
            }
        }
    };
}
pub(crate) use string_enum;

string_enum!(
    /// Integration lifecycle (ADR 0028).
    IntegrationStatus {
        /// Bound to a source and target commit; blockers prevent approval.
        Open => "open",
        /// The target branch has commits the source lacks, and merging them
        /// conflicts. Resolution happens in the workspace, never on the target.
        Conflicted => "conflicted",
        /// A merge of the target into the workspace is in progress there;
        /// the operator (or an assigned agent) resolves the conflicts.
        Resolving => "resolving",
        /// Validation commands are running for the bound source commit.
        Validating => "validating",
        /// No blockers: waiting for the operator to approve the exact commits.
        Ready => "ready",
        /// Approved; Core is updating the target branch.
        Integrating => "integrating",
        /// The target branch points to the integration's merge commit.
        Integrated => "integrated",
        /// Integration was attempted and did not happen (see the reason).
        /// Refreshing reopens it.
        Failed => "failed",
        /// The operator rejected or withdrew the integration.
        Cancelled => "cancelled",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal integration transition {from} → {to}")]
pub struct TransitionError {
    pub from: IntegrationStatus,
    pub to: IntegrationStatus,
}

impl IntegrationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Integrated | Self::Cancelled)
    }

    /// Core is doing something that must not be interleaved.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Validating | Self::Integrating)
    }

    pub fn transition(self, to: Self) -> Result<Self, TransitionError> {
        use IntegrationStatus::*;
        let ok = match (self, to) {
            (a, b) if a == b => false,
            // Re-preparation settles an idle integration to what the facts say.
            (Open | Conflicted | Ready | Failed, Open | Conflicted | Ready) => true,
            (Open | Conflicted | Ready | Failed, Resolving | Validating) => true,
            (Resolving, Open | Conflicted | Ready) => true,
            (Validating, Open | Conflicted | Ready | Failed) => true,
            (Ready, Integrating) => true,
            (Integrating, Integrated | Failed) => true,
            (a, Cancelled) if !a.is_terminal() && a != Integrating => true,
            _ => false,
        };
        if ok { Ok(to) } else { Err(TransitionError { from: self, to }) }
    }
}

/// The message of the merge commit Core creates. The trailers make the
/// integration traceable from Git history alone.
#[allow(clippy::too_many_arguments)]
pub fn merge_message(
    integration_id: Uuid,
    workspace_name: &str,
    source_branch: &str,
    target_branch: &str,
    source_commit: &str,
    target_commit: &str,
    task: Option<(Uuid, &str)>,
    approved_by: &str,
) -> String {
    let title = match task {
        Some((_, t)) => format!("Merge {source_branch} into {target_branch}: {}", one_line(t, 120)),
        None => format!("Merge {source_branch} into {target_branch}"),
    };
    let mut m = format!(
        "{title}\n\nIntegrated by Switchyard from workspace \"{}\".\n\n\
         Switchyard-Integration: {integration_id}\n",
        one_line(workspace_name, 120)
    );
    if let Some((id, _)) = task {
        m.push_str(&format!("Switchyard-Task: {id}\n"));
    }
    m.push_str(&format!(
        "Switchyard-Source: {source_commit}\nSwitchyard-Target-Base: {target_commit}\nSwitchyard-Approved-By: {}\n",
        one_line(approved_by, 120)
    ));
    m
}

fn one_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    line.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use IntegrationStatus::*;

    #[test]
    fn the_documented_paths_are_legal() {
        for path in [
            vec![Open, Validating, Ready, Integrating, Integrated],
            vec![Open, Conflicted, Resolving, Open, Validating, Open],
            vec![Ready, Integrating, Failed, Open, Cancelled],
        ] {
            let mut s = path[0];
            for next in &path[1..] {
                s = s.transition(*next).unwrap_or_else(|e| panic!("{e}"));
            }
        }
    }

    #[test]
    fn integration_needs_ready_and_terminal_states_are_final() {
        for s in [Open, Conflicted, Resolving, Validating, Failed] {
            assert!(s.transition(Integrating).is_err(), "{s} → integrating");
        }
        for s in [Integrated, Cancelled] {
            for t in IntegrationStatus::ALL {
                assert!(s.transition(*t).is_err(), "{s} → {t}");
            }
        }
        assert!(Integrating.transition(Cancelled).is_err(), "a running ref update cannot be cancelled");
        assert!(Ready.transition(Integrated).is_err());
    }

    #[test]
    fn the_merge_message_carries_trailers() {
        let id = Uuid::nil();
        let m = merge_message(
            id,
            "ws",
            "switchyard/x",
            "main",
            "aaa",
            "bbb",
            Some((id, "Fix it\nmore")),
            "desktop",
        );
        assert!(m.starts_with("Merge switchyard/x into main: Fix it\n\n"));
        for t in [
            "Switchyard-Integration: ",
            "Switchyard-Task: ",
            "Switchyard-Source: aaa",
            "Switchyard-Target-Base: bbb",
        ] {
            assert!(m.contains(t), "{t}");
        }
    }
}
