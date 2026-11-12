//! Provider-neutral handle to one supervised agent process.
//!
//! Each adapter runs a driver task that owns the child process and its pipes.
//! Core talks to every adapter through [`AgentSession`] and receives
//! [`SessionUpdate`]s; no provider protocol crosses this boundary.

use tokio::sync::{mpsc, oneshot};

use crate::event::AdapterOutput;
use crate::model::PermissionDecision;
use crate::process::{ExitInfo, ProcessError};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("the agent process is no longer running")]
    Closed,
    #[error("no pending permission request {0}")]
    UnknownPermission(String),
    #[error("cannot write to the agent: {0}")]
    Write(String),
}

/// Final report when the process ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub exit: ExitInfo,
    /// Core asked for the stop (as opposed to the agent exiting on its own).
    pub stop_requested: bool,
    /// Last bytes of stderr, for diagnostics.
    pub stderr_tail: String,
    pub malformed_lines: u64,
    /// The adapter ended the session because it could not continue (e.g. a
    /// thread that could not be resumed). Always reported as a failure.
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionUpdate {
    Output(AdapterOutput),
    /// Always the last update.
    Exited(ExitReport),
}

pub(crate) enum Command {
    Send {
        text: String,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Interrupt {
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Decide {
        request_id: String,
        decision: PermissionDecision,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Stop {
        reply: oneshot::Sender<ExitInfo>,
    },
}

/// Handle to a running agent process, whatever the provider. Dropping every
/// handle stops it.
#[derive(Clone)]
pub struct AgentSession {
    commands: mpsc::Sender<Command>,
    pid: u32,
}

impl AgentSession {
    pub(crate) fn new(commands: mpsc::Sender<Command>, pid: u32) -> Self {
        Self { commands, pid }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    async fn request(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<(), SessionError>>) -> Command,
    ) -> Result<(), SessionError> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(make(tx)).await.map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)?
    }

    /// Sends a user message. If a turn is running, the adapter queues it as
    /// the next turn (only for adapters that support queueing).
    pub async fn send_message(&self, text: String) -> Result<(), SessionError> {
        self.request(|reply| Command::Send { text, reply }).await
    }

    pub async fn interrupt(&self) -> Result<(), SessionError> {
        self.request(|reply| Command::Interrupt { reply }).await
    }

    pub async fn decide(&self, request_id: String, decision: PermissionDecision) -> Result<(), SessionError> {
        self.request(|reply| Command::Decide { request_id, decision, reply }).await
    }

    /// Closes stdin, then escalates to SIGTERM/SIGKILL after the configured
    /// grace periods. Resolves when the process has exited.
    pub async fn stop(&self) -> Result<ExitInfo, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Stop { reply: tx }).await.map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)
    }
}
