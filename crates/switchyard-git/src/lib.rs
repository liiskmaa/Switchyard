//! Git inspection and worktree management through the `git` CLI.
//!
//! Switchyard shells out to the user's `git` rather than linking a Git
//! library: it honours the user's configuration, credential helpers and
//! hooks exactly like the agents (which also use the CLI) will, and avoids a
//! large native dependency. Every invocation is time-bounded and never
//! prompts.
//!
//! The only mutating operations are `git worktree add` (with a new branch)
//! and `git worktree remove` without `--force` for Core-owned workspaces
//! ([`worktree`]). Nothing here merges, rebases, deletes branches, prunes or
//! discards changes.

mod command;
mod error;
mod ignore;
mod repository;
mod status;
pub mod worktree;

pub use command::{Git, GitConfig};
pub use error::GitError;
pub use repository::RepositoryRoot;
pub use status::{GitStatus, parse_porcelain_v2};
