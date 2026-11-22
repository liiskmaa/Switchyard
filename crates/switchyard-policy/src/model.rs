//! Policy vocabulary: operation categories, decisions, normalized operations,
//! path facts and rules.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the compiled-in system restrictions and built-in defaults.
/// Recorded with every evaluation; bump whenever either changes.
pub const SYSTEM_POLICY_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// What an operation does, independent of the agent vendor.
    OperationCategory {
        FileRead => "file_read",
        FileWrite => "file_write",
        FileEdit => "file_edit",
        FileDelete => "file_delete",
        DirectoryList => "directory_list",
        Search => "search",
        /// Arbitrary command execution. Its effects cannot be classified.
        Shell => "shell",
        Network => "network",
        /// Tools provided by an external server (e.g. MCP).
        ExternalTool => "external_tool",
        /// Unrecognized or unsupported. Always denied.
        Unknown => "unknown",
    }
);

impl OperationCategory {
    /// Operations whose targets are file system paths.
    pub fn is_path_category(self) -> bool {
        matches!(
            self,
            Self::FileRead
                | Self::FileWrite
                | Self::FileEdit
                | Self::FileDelete
                | Self::DirectoryList
                | Self::Search
        )
    }

    /// Operations that change files.
    pub fn is_mutation(self) -> bool {
        matches!(self, Self::FileWrite | Self::FileEdit | Self::FileDelete)
    }

    /// Whether configured rules may target this category at all.
    pub fn configurable(self) -> bool {
        self != Self::Unknown
    }

    /// Whether an `allow` rule is supported for this category.
    pub fn allow_supported(self) -> bool {
        self.is_path_category() || self == Self::Shell
    }

    /// The built-in default when no configured rule matches (before caps).
    pub fn default_decision(self) -> Decision {
        match self {
            Self::FileRead | Self::Search | Self::DirectoryList => Decision::Allow,
            Self::FileWrite | Self::FileEdit | Self::FileDelete | Self::Shell => Decision::Ask,
            // No current adapter enables these tools; a request for one is
            // unexpected. A rule may relax this to `ask`, never to `allow`.
            Self::Network | Self::ExternalTool | Self::Unknown => Decision::Deny,
        }
    }

    /// Lower-case phrase used in explanations ("reading files").
    pub fn phrase(self) -> &'static str {
        match self {
            Self::FileRead => "reading files",
            Self::FileWrite => "writing files",
            Self::FileEdit => "editing files",
            Self::FileDelete => "deleting files",
            Self::DirectoryList => "listing directories",
            Self::Search => "searching files",
            Self::Shell => "running shell commands",
            Self::Network => "network access",
            Self::ExternalTool => "external tools",
            Self::Unknown => "unrecognized operations",
        }
    }
}

string_enum!(
    /// Outcome of an evaluation, and the effect of a rule.
    Decision {
        /// Permit without asking a person.
        Allow => "allow",
        /// Create a pending approval for a person to decide.
        Ask => "ask",
        /// Reject; no approval is offered.
        Deny => "deny",
    }
);

impl Decision {
    /// Higher is more restrictive.
    pub fn restrictiveness(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Deny => 2,
        }
    }

    pub fn most_restrictive(self, other: Self) -> Self {
        if other.restrictiveness() > self.restrictiveness() { other } else { self }
    }
}

string_enum!(
    /// Where a rule applies.
    RuleScope {
        Project => "project",
        Workspace => "workspace",
    }
);

/// A permission request in adapter-neutral form. Produced by the adapter's
/// classifier; Core attaches project, workspace and session from its own
/// records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Adapter identity, e.g. `claude_code`.
    pub adapter: String,
    /// The vendor's tool name (display and audit only; never matched).
    pub tool: String,
    pub category: OperationCategory,
    /// Target paths exactly as requested.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Shell command text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The command keeps running after the tool returns.
    #[serde(default)]
    pub background: bool,
    /// Input field names the adapter does not know.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpected_fields: Vec<String>,
    /// Why the request could not be classified. Always denied when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

impl Operation {
    /// An operation that could not be classified (denied by the system).
    pub fn unclassified(
        adapter: &str,
        tool: &str,
        category: OperationCategory,
        problem: impl Into<String>,
    ) -> Self {
        Self {
            adapter: adapter.into(),
            tool: tool.into(),
            category,
            paths: Vec::new(),
            command: None,
            background: false,
            unexpected_fields: Vec::new(),
            problem: Some(problem.into()),
        }
    }
}

/// Where a target path lies relative to the session's workspace, as
/// established by Core before evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "location", rename_all = "snake_case")]
pub enum PathFact {
    /// Inside the workspace. `lexical` is the normalized requested path,
    /// `resolved` the path after following symlinks; both relative to the
    /// workspace root (`""` is the root itself), `/`-separated.
    Inside {
        lexical: String,
        resolved: String,
    },
    Outside,
    Unresolved {
        reason: String,
    },
}

/// Identity established by Core from its own records, never from the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScope {
    pub project_id: Uuid,
    pub session_id: Uuid,
    /// The session's workspace.
    pub workspace_id: Option<Uuid>,
    /// The project that workspace belongs to (must equal `project_id`).
    pub workspace_project_id: Option<Uuid>,
    /// The session is a review session (ADR 0027): file changes are denied
    /// and commands are never allowed automatically. Absent in evaluations
    /// recorded before milestone 6.
    #[serde(default)]
    pub review_session: bool,
    /// The project's auto mode (ADR 0037), read by Core in the transaction
    /// that evaluates: file changes and shell commands inside the workspace
    /// are allowed when no configured rule decides. Absent in evaluations
    /// recorded before it existed.
    #[serde(default)]
    pub auto_mode: bool,
}

/// Everything the evaluator looks at for one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationInput {
    pub operation: Operation,
    /// One fact per entry of `operation.paths`, in order. Search operations
    /// without an explicit path carry one fact for the workspace root.
    pub paths: Vec<PathFact>,
    pub scope: RequestScope,
}

/// Structured matching conditions. Nothing else is accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    /// Relative directory or file inside the workspace (path categories).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// Exact command (`shell`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        self.path_prefix.is_none() && self.command.is_none()
    }
}

/// A configured rule as the evaluator sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: Uuid,
    pub revision: u32,
    pub project_id: Uuid,
    /// `None` = project rule.
    pub workspace_id: Option<Uuid>,
    pub category: OperationCategory,
    pub decision: Decision,
    pub conditions: Conditions,
    pub description: String,
    pub enabled: bool,
    pub archived: bool,
    pub created_at_ms: i64,
}

impl Rule {
    pub fn scope(&self) -> RuleScope {
        if self.workspace_id.is_some() { RuleScope::Workspace } else { RuleScope::Project }
    }
}
