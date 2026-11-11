//! Read-only Git inspection through the `git` CLI.
//!
//! Switchyard shells out to the user's `git` rather than linking a Git
//! library: it honours the user's configuration, credential helpers and
//! hooks exactly like the agents (which also use the CLI) will, and avoids a
//! large native dependency. Every invocation is time-bounded and never
//! prompts.
//!
//! This milestone performs no mutating Git operations.

mod command;
mod error;
mod repository;
mod status;

pub use command::{Git, GitConfig};
pub use error::GitError;
pub use repository::RepositoryRoot;
pub use status::{GitStatus, parse_porcelain_v2};
