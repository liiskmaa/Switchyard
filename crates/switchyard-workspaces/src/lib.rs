//! Workspace domain model.
//!
//! A *project* is a registered source repository. A *workspace* is an
//! isolated working copy of it (a Git worktree on its own branch) that Core
//! owns. A *session* is one agent execution attached to exactly one
//! workspace. This crate holds the pure rules shared by storage, protocol and
//! Core: the lifecycle and its legal transitions, naming and on-disk layout,
//! and the cleanup safety evaluation. It performs no I/O.
//!
//! Workspaces isolate *source trees*, not processes: an agent command still
//! runs with the user's privileges and can reach files outside its
//! workspace.

mod cleanup;
mod layout;
mod model;

pub use cleanup::{
    CleanupFinding, CleanupInputs, CleanupReport, RegisteredWorktree, TreeProblem, TreeSummary,
    evaluate_cleanup,
};
pub use layout::{
    BRANCH_PREFIX, MAX_BASE_REF_BYTES, MAX_NAME_CHARS, NameError, branch_name, validate_base_ref,
    validate_name, worktree_path,
};
pub use model::{TransitionError, UnknownValue, WorkspaceKind, WorkspaceStatus};
