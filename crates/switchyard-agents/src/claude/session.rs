//! One supervised Claude Code process.
//!
//! A single driver task owns the child, its stdin and stdout. Commands
//! (send, interrupt, decide, stop) arrive over a bounded channel; normalized
//! output leaves over another bounded channel. stderr is drained into a small
//! tail buffer used only for failure diagnostics — it is never logged
//! wholesale, since tools may print sensitive data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use super::launch::{LaunchSpec, build_args, build_env};
use super::wire::Normalizer;
use crate::event::{AdapterOutput, AgentEvent};
use crate::model::PermissionDecision;
use crate::process::{self, ExitInfo, Line, ProcessError, ProcessSpec, TailBuffer};

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub executable: PathBuf,
    /// After closing stdin on stop, how long to wait before SIGTERM.
    pub stop_grace: Duration,
    /// After SIGTERM, how long to wait before SIGKILL.
    pub term_grace: Duration,
    /// Longest stdout line accepted (tool results can be large).
    pub max_line_bytes: usize,
    /// Capacity of the update channel to Core.
    pub update_capacity: usize,
}

impl ClaudeConfig {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            stop_grace: Duration::from_secs(5),
            term_grace: Duration::from_secs(5),
            max_line_bytes: 16 * 1024 * 1024,
            update_capacity: 256,
        }
    }
}

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
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionUpdate {
    Output(AdapterOutput),
    /// Always the last update.
    Exited(ExitReport),
}

enum Command {
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

/// Handle to a running Claude process. Dropping every handle stops it.
#[derive(Clone)]
pub struct ClaudeSession {
    commands: mpsc::Sender<Command>,
    pid: u32,
}

impl ClaudeSession {
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

    /// Sends a user message. If a turn is running, Claude queues it as the
    /// next turn.
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

/// Launches Claude for `spec`. Updates are delivered on the returned
/// receiver until (and including) [`SessionUpdate::Exited`].
pub fn spawn_session(
    config: &ClaudeConfig,
    spec: &LaunchSpec,
) -> Result<(ClaudeSession, mpsc::Receiver<SessionUpdate>), SessionError> {
    let process_spec = ProcessSpec {
        program: config.executable.clone(),
        args: build_args(spec),
        cwd: spec.cwd.clone(),
        env: build_env(std::env::vars_os(), &config.executable),
    };
    let mut child = process::spawn(&process_spec)?;
    let pid = child.id().ok_or(ProcessError::MissingPipe("pid"))?;
    let stdin = child.stdin.take().ok_or(ProcessError::MissingPipe("stdin"))?;
    let stdout = child.stdout.take().ok_or(ProcessError::MissingPipe("stdout"))?;
    let stderr = child.stderr.take().ok_or(ProcessError::MissingPipe("stderr"))?;

    let tail = Arc::new(Mutex::new(TailBuffer::new(4096)));
    let stderr_tail = Arc::clone(&tail);
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut t) = stderr_tail.lock() {
                        t.push(&buf[..n]);
                    }
                }
            }
        }
    });

    let (commands, command_rx) = mpsc::channel(32);
    let (updates, update_rx) = mpsc::channel(config.update_capacity.max(1));
    tracing::info!(pid, cwd = %spec.cwd.display(), "Claude Code process started");
    tokio::spawn(drive(
        Driver {
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            normalizer: Normalizer::new(),
            pending_inputs: HashMap::new(),
            updates,
            tail,
            config: config.clone(),
            malformed: 0,
        },
        child,
        command_rx,
    ));
    Ok((ClaudeSession { commands, pid }, update_rx))
}

struct Driver {
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    normalizer: Normalizer,
    /// native request id → original tool input (echoed on allow).
    pending_inputs: HashMap<String, Value>,
    updates: mpsc::Sender<SessionUpdate>,
    tail: Arc<Mutex<TailBuffer>>,
    config: ClaudeConfig,
    malformed: u64,
}

impl Driver {
    async fn write(&mut self, message: &Value) -> Result<(), SessionError> {
        let stdin = self.stdin.as_mut().ok_or(SessionError::Closed)?;
        let mut bytes = serde_json::to_vec(message).map_err(|e| SessionError::Write(e.to_string()))?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await.map_err(|e| SessionError::Write(e.to_string()))?;
        stdin.flush().await.map_err(|e| SessionError::Write(e.to_string()))
    }

    async fn handle(&mut self, command: Command) -> Option<oneshot::Sender<ExitInfo>> {
        match command {
            Command::Send { text, reply } => {
                let message = self.normalizer.encode_user_message(&text);
                let _ = reply.send(self.write(&message).await);
            }
            Command::Interrupt { reply } => {
                let message = self.normalizer.encode_interrupt();
                let _ = reply.send(self.write(&message).await);
            }
            Command::Decide { request_id, decision, reply } => {
                let result = match self.pending_inputs.remove(&request_id) {
                    None => Err(SessionError::UnknownPermission(request_id)),
                    Some(input) => {
                        let message = self.normalizer.encode_permission(&request_id, &decision, &input);
                        self.write(&message).await
                    }
                };
                let _ = reply.send(result);
            }
            Command::Stop { reply } => return Some(reply),
        }
        None
    }

    /// Forwards one stdout line. Returns `false` at end of stream.
    async fn pump(&mut self, line: std::io::Result<Option<Line>>) -> bool {
        let outputs = match line {
            Ok(Some(Line::Complete(bytes))) => {
                let outputs = self.normalizer.ingest(&bytes);
                if matches!(&outputs[..], [AdapterOutput::Event(AgentEvent::Warning { message })] if message.starts_with("unparseable"))
                {
                    self.malformed += 1;
                }
                outputs
            }
            Ok(Some(Line::TooLong { bytes })) => {
                self.malformed += 1;
                vec![AdapterOutput::Event(AgentEvent::Warning {
                    message: format!(
                        "discarded an output line of {bytes} bytes (limit {})",
                        self.config.max_line_bytes
                    ),
                })]
            }
            Ok(None) => return false,
            Err(err) => {
                tracing::warn!(%err, "reading agent stdout failed");
                return false;
            }
        };
        for output in outputs {
            match &output {
                AdapterOutput::PermissionRequested(p) => {
                    self.pending_inputs.insert(p.native_request_id.clone(), p.input.clone());
                }
                AdapterOutput::PermissionCancelled { native_request_id } => {
                    self.pending_inputs.remove(native_request_id);
                }
                _ => {}
            }
            // Bounded: if Core is slow, stop reading stdout (the agent then
            // blocks on its own output) instead of buffering without limit.
            if self.updates.send(SessionUpdate::Output(output)).await.is_err() {
                tracing::warn!("session update receiver dropped");
            }
        }
        true
    }
}

async fn drive(mut d: Driver, mut child: Child, mut commands: mpsc::Receiver<Command>) {
    let max = d.config.max_line_bytes;
    let mut stop_reply = None;
    let mut stop_requested = false;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if let Some(reply) = d.handle(command).await {
                        stop_reply = Some(reply);
                        stop_requested = true;
                        break;
                    }
                }
                // Every handle dropped: treat as a stop request.
                None => { stop_requested = true; break; }
            },
            line = process::read_line(&mut d.stdout, max) => {
                if !d.pump(line).await { break; }
            }
        }
    }
    // Further commands fail with `Closed`.
    drop(commands);

    // Closing stdin tells Claude no more input will come; it exits after the
    // current turn. Keep draining stdout meanwhile so it never blocks on a
    // full pipe; escalate to SIGTERM/SIGKILL if it does not exit in time.
    drop(d.stdin.take());
    let grace = if stop_requested { d.config.stop_grace } else { Duration::from_secs(10) };
    let exit = {
        let terminate = process::terminate(&mut child, grace, d.config.term_grace);
        tokio::pin!(terminate);
        let mut stdout_open = true;
        loop {
            tokio::select! {
                exit = &mut terminate => break exit,
                line = process::read_line(&mut d.stdout, max), if stdout_open => {
                    stdout_open = d.pump(line).await;
                }
            }
        }
    };
    // Flush output still buffered in the pipe. Bounded, because a grandchild
    // (e.g. a background shell job) may keep the pipe open.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let line = process::read_line(&mut d.stdout, max).await;
            if !d.pump(line).await {
                break;
            }
        }
    })
    .await;

    let stderr_tail = d.tail.lock().map(|t| t.contents()).unwrap_or_default();
    tracing::info!(exit = %exit.describe(), stop_requested, "Claude Code process exited");
    let report = ExitReport { exit, stop_requested, stderr_tail, malformed_lines: d.malformed };
    if d.updates.send(SessionUpdate::Exited(report)).await.is_err() {
        tracing::warn!("session update receiver dropped before exit report");
    }
    if let Some(reply) = stop_reply {
        let _ = reply.send(exit);
    }
}
