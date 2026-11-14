//! Git inspection and worktree management through the `git` CLI.
//!
//! Switchyard shells out to the user's `git` rather than linking a Git
//! library: it honours the user's configuration, credential helpers and
//! hooks exactly like the agents (which also use the CLI) will, and avoids a
//! large native dependency. Every invocation is time-bounded and never
//! prompts.
//!
//! The mutating operations are `git worktree add` (with a new branch) and
//! `git worktree remove` without `--force` for Core-owned workspaces
//! ([`worktree`]), and the integration operations in [`integrate`]: object
//! creation, one compare-and-swap branch update, commits and merges inside
//! Core-owned workspaces, and a fast-forward of a clean target checkout.
//! Nothing here pushes, rebases, resets, force-updates, deletes branches,
//! prunes or discards user changes.

mod command;
mod error;
mod ignore;
pub mod integrate;
mod repository;
mod status;
pub mod worktree;

pub use command::{Git, GitConfig};
pub use error::GitError;
pub use integrate::{
    ChangeKind, ChangedFile, CommitInfo, DiffSummary, FastForwardError, MergeOutcome, RepoState,
    WorktreeMerge, conflict_blocks, looks_binary,
};
pub use repository::RepositoryRoot;
pub use status::{GitStatus, parse_porcelain_v2};
