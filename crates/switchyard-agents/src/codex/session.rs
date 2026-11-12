//! One supervised `codex app-server` process.
//!
//! A single driver task owns the child, its stdin and stdout, and the
//! [`Protocol`] state. Commands arrive from Core over a bounded channel;
//! normalized output leaves over another bounded channel. stderr is drained
//! into a small tail buffer used only for failure diagnostics.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use super::launch::{LaunchSpec, build_args, build_env};
use super::protocol::{Protocol, Step, ThreadMode};
use crate::event::{AdapterOutput, AgentEvent};
use crate::process::{self, ExitInfo, Line, ProcessError, ProcessSpec, TailBuffer};
use crate::session::{AgentSession, Command, ExitReport, SessionError, SessionUpdate};

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub executable: PathBuf,
    /// After closing stdin on stop, how long to wait before SIGTERM.
    pub stop_grace: Duration,
    /// After SIGTERM, how long to wait before SIGKILL.
    pub term_grace: Duration,
    /// Longest stdout line accepted (command output can be large).
    pub max_line_bytes: usize,
    /// Capacity of the update channel to Core.
    pub update_capacity: usize,
    /// Reported to Codex as the client version.
    pub client_version: String,
}

impl CodexConfig {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            stop_grace: Duration::from_secs(5),
            term_grace: Duration::from_secs(5),
            max_line_bytes: 16 * 1024 * 1024,
            update_capacity: 256,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Launches `codex app-server` for `spec` and starts the handshake. Updates
/// are delivered on the returned receiver until (and including)
/// [`SessionUpdate::Exited`].
pub fn spawn_session(
    config: &CodexConfig,
    spec: &LaunchSpec,
) -> Result<(AgentSession, mpsc::Receiver<SessionUpdate>), SessionError> {
    let process_spec = ProcessSpec {
        program: config.executable.clone(),
        args: build_args(),
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

    let mode = match &spec.resume_thread {
        Some(id) => ThreadMode::Resume(id.clone()),
        None => ThreadMode::New,
    };
    let protocol =
        Protocol::new(mode, spec.cwd.display().to_string(), spec.model.clone(), &config.client_version);
    let (commands, command_rx) = mpsc::channel(32);
    let (updates, update_rx) = mpsc::channel(config.update_capacity.max(1));
    tracing::info!(pid, cwd = %spec.cwd.display(), resume = spec.resume_thread.is_some(), "Codex app-server started");
    tokio::spawn(drive(
        Driver {
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            protocol,
            updates,
            tail,
            config: config.clone(),
            malformed: 0,
            failure: None,
        },
        child,
        command_rx,
    ));
    Ok((AgentSession::new(commands, pid), update_rx))
}

struct Driver {
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    protocol: Protocol,
    updates: mpsc::Sender<SessionUpdate>,
    tail: Arc<Mutex<TailBuffer>>,
    config: CodexConfig,
    malformed: u64,
    /// Set when the protocol cannot continue; ends the process.
    failure: Option<String>,
}

impl Driver {
    async fn write(&mut self, message: &Value) -> Result<(), SessionError> {
        let stdin = self.stdin.as_mut().ok_or(SessionError::Closed)?;
        let mut bytes = serde_json::to_vec(message).map_err(|e| SessionError::Write(e.to_string()))?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await.map_err(|e| SessionError::Write(e.to_string()))?;
        stdin.flush().await.map_err(|e| SessionError::Write(e.to_string()))
    }

    /// Writes the step's messages and forwards its outputs.
    async fn apply(&mut self, step: Step) -> Result<(), SessionError> {
        let mut result = Ok(());
        for w in &step.writes {
            if let Err(e) = self.write(w).await {
                result = Err(e);
                break;
            }
        }
        self.forward(step.outputs).await;
        if let Some(f) = step.fatal
            && self.failure.is_none()
        {
            tracing::warn!(failure = %f, "Codex session cannot continue");
            self.failure = Some(f);
        }
        result
    }

    async fn forward(&mut self, outputs: Vec<AdapterOutput>) {
        for output in outputs {
            if matches!(&output, AdapterOutput::Event(AgentEvent::Warning { message }) if message.starts_with("unparseable"))
            {
                self.malformed += 1;
            }
            // Bounded: if Core is slow, stop reading stdout (Codex then blocks
            // on its own output) instead of buffering without limit.
            if self.updates.send(SessionUpdate::Output(output)).await.is_err() {
                tracing::warn!("session update receiver dropped");
            }
        }
    }

    async fn handle(&mut self, command: Command) -> Option<oneshot::Sender<ExitInfo>> {
        match command {
            Command::Send { text, reply } => {
                let step = self.protocol.send_user_message(text);
                let result = match &step.fatal {
                    Some(f) => Err(SessionError::Write(f.clone())),
                    None => Ok(()),
                };
                let applied = self.apply(Step { fatal: None, ..step }).await;
                let _ = reply.send(result.and(applied));
            }
            Command::Interrupt { reply } => {
                let result = match self.protocol.interrupt() {
                    Ok(step) => self.apply(step).await,
                    Err(e) => Err(SessionError::Write(e)),
                };
                let _ = reply.send(result);
            }
            Command::Decide { request_id, decision, reply } => {
                let result = match self.protocol.decide(&request_id, &decision) {
                    Ok(message) => self.write(&message).await,
                    Err(_) => Err(SessionError::UnknownPermission(request_id)),
                };
                let _ = reply.send(result);
            }
            Command::Stop { reply } => return Some(reply),
        }
        None
    }

    /// Processes one stdout line. Returns `false` at end of stream.
    async fn pump(&mut self, line: std::io::Result<Option<Line>>) -> bool {
        match line {
            Ok(Some(Line::Complete(bytes))) => {
                let step = self.protocol.ingest(&bytes);
                if let Err(e) = self.apply(step).await {
                    tracing::warn!(%e, "cannot write to Codex");
                }
            }
            Ok(Some(Line::TooLong { bytes })) => {
                self.malformed += 1;
                self.forward(vec![AdapterOutput::Event(AgentEvent::Warning {
                    message: format!(
                        "discarded an output line of {bytes} bytes (limit {})",
                        self.config.max_line_bytes
                    ),
                })])
                .await;
            }
            Ok(None) => return false,
            Err(err) => {
                tracing::warn!(%err, "reading Codex stdout failed");
                return false;
            }
        }
        true
    }
}

async fn drive(mut d: Driver, mut child: Child, mut commands: mpsc::Receiver<Command>) {
    let max = d.config.max_line_bytes;
    let mut stop_reply = None;
    let mut stop_requested = false;
    let opening = d.protocol.start();
    let mut alive = true;
    for m in &opening {
        if let Err(e) = d.write(m).await {
            tracing::warn!(%e, "cannot send initialize to Codex");
            d.failure = Some(format!("cannot initialize the Codex app-server: {e}"));
            alive = false;
        }
    }
    while alive && d.failure.is_none() {
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
    drop(commands);

    // Closing stdin ends the app-server (verified: ~0.1 s). Keep draining
    // stdout meanwhile; escalate to SIGTERM/SIGKILL if it does not exit.
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
    tracing::info!(exit = %exit.describe(), stop_requested, failure = ?d.failure, "Codex app-server exited");
    let report = ExitReport {
        exit,
        stop_requested,
        stderr_tail,
        malformed_lines: d.malformed,
        failure: if stop_requested { None } else { d.failure.take() },
    };
    if d.updates.send(SessionUpdate::Exited(report)).await.is_err() {
        tracing::warn!("session update receiver dropped before exit report");
    }
    if let Some(reply) = stop_reply {
        let _ = reply.send(exit);
    }
}
