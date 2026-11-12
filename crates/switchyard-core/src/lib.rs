//! Switchyard Core.
//!
//! Core is the long-running owner of all orchestration state: persistence,
//! the event engine, project registrations, workspaces (Git worktrees),
//! agent processes and the approval policy that answers their permission
//! requests. Clients such as the desktop app connect over a Unix domain
//! socket; Core's lifetime is independent of any client.
//!
//! [`start`] brings Core up and returns a [`RunningCore`] handle; the binary
//! in `main.rs` adds argument parsing, logging and signal handling.

mod agents;
pub mod config;
mod connection;
mod error;
mod handlers;
mod journal;
mod lifecycle;
pub mod logging;
mod policy;
mod server;
mod state;
mod workspaces;

pub use config::CoreConfig;
pub use error::CoreError;
pub use lifecycle::{RunningCore, start};

/// Version of this Core build.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
