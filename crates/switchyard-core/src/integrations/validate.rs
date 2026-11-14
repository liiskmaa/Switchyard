//! Validation: the project's configured commands, run for one exact source
//! commit (ADR 0030). A local runner, not CI.
//!
//! Every command is evaluated by the same policy evaluator and rules as an
//! agent's shell command: `deny` never runs, `ask` runs only if the operator
//! authorized it for this run, `allow` runs. Commands run directly (never
//! through a shell), in the workspace, in their own process group so that
//! cancellation and timeouts stop everything they started.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use switchyard_events::{DomainEvent, new_id, now_ms};
use switchyard_integrations::{
    IntegrationStatus, MAX_OUTPUT_BYTES, RunStatus, StepStatus, command_line, final_run_status,
};
use switchyard_policy::{Decision, EvaluationInput, Operation, OperationCategory, RequestScope, evaluate};
use switchyard_protocol::methods::IntegrationDetail;
use switchyard_protocol::model::{Integration, ValidationPlanEntry, ValidationRunInfo, ValidationStepInfo};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{
    self as store, CommandRecord, IntegrationRecord, IntegrationUpdate, RunRecord, StepOutcome, StepRecord,
};
use switchyard_storage::{Connection, StorageError, policies};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;

use super::{IntegrationService, Op, OpKind, context, invalid_state, transition, wire_from_conn};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

/// Blockers that do not prevent validating the source commit.
const VALIDATABLE: &[&str] = &[
    "validation_required",
    "validation_failed",
    "review_required",
    "target_checkout_dirty",
    "target_checkout_busy",
    "identity_missing",
    "task_not_eligible",
];
/// Grace between SIGTERM and SIGKILL for a stopped command.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// How long output is still collected after the command's group ended.
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// The policy's answer for running `argv` as a validation command in the workspace.
fn evaluate_command(
    c: &Connection,
    project_id: Uuid,
    workspace_id: Uuid,
    argv: &[String],
) -> Result<switchyard_policy::Evaluation, StorageError> {
    let input = EvaluationInput {
        operation: Operation {
            adapter: "switchyard".into(),
            tool: "validation".into(),
            category: OperationCategory::Shell,
            paths: Vec::new(),
            command: Some(command_line(argv)),
            background: false,
            unexpected_fields: Vec::new(),
            problem: None,
        },
        paths: Vec::new(),
        scope: RequestScope {
            project_id,
            // No agent session: Core runs the command on the operator's request.
            session_id: Uuid::nil(),
            workspace_id: Some(workspace_id),
            workspace_project_id: Some(project_id),
            review_session: false,
        },
    };
    let rules = policies::applicable_rules(c, project_id, Some(workspace_id))?;
    let revision = policies::current_revision(c)?;
    Ok(evaluate(&input, &rules, revision))
}

fn step_wire(s: StepRecord) -> ValidationStepInfo {
    ValidationStepInfo {
        id: s.id,
        position: s.position,
        name: s.name,
        command_line: s.command_line,
        cwd: s.cwd,
        timeout_secs: s.timeout_secs,
        policy_decision: s.policy_decision,
        policy_explanation: s.policy_explanation,
        authorized_by: s.authorized_by,
        status: s.status,
        exit_code: s.exit_code,
        signal: s.signal,
        stdout: s.stdout,
        stderr: s.stderr,
        stdout_truncated: s.stdout_truncated,
        stderr_truncated: s.stderr_truncated,
        detail: s.detail,
        started_at_ms: s.started_at_ms,
        ended_at_ms: s.ended_at_ms,
    }
}

fn run_wire(c: &Connection, r: RunRecord) -> Result<ValidationRunInfo, StorageError> {
    Ok(ValidationRunInfo {
        steps: store::steps_for_run(c, r.id)?.into_iter().map(step_wire).collect(),
        id: r.id,
        source_commit: r.source_commit,
        status: r.status,
        requested_by: r.requested_by,
        started_at_ms: r.started_at_ms,
        ended_at_ms: r.ended_at_ms,
    })
}

/// One line of a run's configuration: what it ran and why it was allowed.
fn config_line(command_id: Uuid, command_line: &str, timeout_secs: u32, decision: &str) -> String {
    format!("{command_id}\t{command_line}\t{timeout_secs}\t{decision}")
}

/// The configuration a run executed, from its steps: a passed run counts
/// only while the project's current configuration ([`validation_config`])
/// is identical.
fn config_of_steps(steps: &[StepRecord]) -> String {
    steps
        .iter()
        .map(|s| {
            config_line(
                s.command_id.unwrap_or_else(Uuid::nil),
                &s.command_line,
                s.timeout_secs,
                &s.policy_decision,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a validation run is valid for besides the source commit: the enabled
/// commands (exact command lines and timeouts) and the policy's decision for
/// each, as a run would record them now.
pub(super) fn validation_config(
    c: &Connection,
    project_id: Uuid,
    workspace_id: Uuid,
) -> Result<String, StorageError> {
    let mut lines = Vec::new();
    for cmd in store::commands(c, project_id)?.into_iter().filter(|cmd| cmd.enabled) {
        let e = evaluate_command(c, project_id, workspace_id, &cmd.argv)?;
        lines.push(config_line(cmd.id, &command_line(&cmd.argv), cmd.timeout_secs, e.decision.as_str()));
    }
    Ok(lines.join("\n"))
}

fn plan(c: &Connection, i: &IntegrationRecord) -> Result<Vec<ValidationPlanEntry>, StorageError> {
    store::commands(c, i.project_id)?
        .into_iter()
        .filter(|cmd| cmd.enabled)
        .map(|cmd| {
            let e = evaluate_command(c, i.project_id, i.workspace_id, &cmd.argv)?;
            Ok(ValidationPlanEntry {
                command_id: cmd.id,
                command_line: command_line(&cmd.argv),
                name: cmd.name,
                policy_decision: e.decision.as_str().into(),
                policy_explanation: e.explanation,
            })
        })
        .collect()
}

/// Keeps the last `MAX_OUTPUT_BYTES` of a stream.
async fn tail<R: AsyncRead + Unpin>(mut r: R) -> (String, bool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_OUTPUT_BYTES {
                    let cut = buf.len() - MAX_OUTPUT_BYTES;
                    buf.drain(..cut);
                    truncated = true;
                }
            }
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

fn signal_group(pid: Option<u32>, signal: Signal) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok())
        && let Err(e) = killpg(Pid::from_raw(pid), signal)
    {
        tracing::debug!(pid, %e, "signalling a validation process group failed");
    }
}

/// Runs one command; never panics, every outcome is a status.
async fn run_command(
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> (StepStatus, StepOutcome) {
    let Some((program, args)) = argv.split_first() else {
        return (
            StepStatus::Error,
            StepOutcome { detail: Some("empty command".into()), ..Default::default() },
        );
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                StepStatus::Error,
                StepOutcome { detail: Some(format!("cannot start {program}: {e}")), ..Default::default() },
            );
        }
    };
    let pid = child.id();
    let out = child.stdout.take().map(|s| tokio::spawn(tail(s)));
    let err = child.stderr.take().map(|s| tokio::spawn(tail(s)));
    enum End {
        Exited(std::io::Result<std::process::ExitStatus>),
        TimedOut,
        Cancelled,
    }
    let end = tokio::select! {
        status = child.wait() => End::Exited(status),
        () = tokio::time::sleep(timeout) => End::TimedOut,
        _ = cancel.wait_for(|c| *c) => End::Cancelled,
    };
    let status = match end {
        End::Exited(s) => s,
        End::TimedOut | End::Cancelled => {
            signal_group(pid, Signal::SIGTERM);
            let waited = match tokio::time::timeout(KILL_GRACE, child.wait()).await {
                Ok(s) => s,
                Err(_) => {
                    signal_group(pid, Signal::SIGKILL);
                    child.wait().await
                }
            };
            // Members that ignored SIGTERM (or outlived the leader) are killed too.
            signal_group(pid, Signal::SIGKILL);
            let st = if matches!(end, End::TimedOut) { StepStatus::TimedOut } else { StepStatus::Cancelled };
            let (stdout, stdout_truncated) = join(out).await;
            let (stderr, stderr_truncated) = join(err).await;
            let detail = if st == StepStatus::TimedOut {
                format!("stopped after the {} s timeout", timeout.as_secs())
            } else {
                "cancelled by the operator".to_owned()
            };
            let (exit_code, signal) = codes(waited.ok());
            return (
                st,
                StepOutcome {
                    exit_code,
                    signal,
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                    stdout_truncated,
                    stderr_truncated,
                    detail: Some(detail),
                },
            );
        }
    };
    // Children left in the group by a command that exited are stopped too.
    signal_group(pid, Signal::SIGKILL);
    let (stdout, stdout_truncated) = join(out).await;
    let (stderr, stderr_truncated) = join(err).await;
    let (exit_code, signal) = codes(status.as_ref().ok().copied());
    let st = match status {
        Ok(s) if s.success() => StepStatus::Passed,
        Ok(_) => StepStatus::Failed,
        Err(_) => StepStatus::Error,
    };
    (
        st,
        StepOutcome {
            exit_code,
            signal,
            stdout: Some(stdout),
            stderr: Some(stderr),
            stdout_truncated,
            stderr_truncated,
            detail: None,
        },
    )
}

/// Collects a stream's output. A process that left the group (e.g. with
/// `setsid`) can keep the pipe open forever: collection stops after a grace.
async fn join(h: Option<tokio::task::JoinHandle<(String, bool)>>) -> (String, bool) {
    let Some(mut h) = h else { return (String::new(), false) };
    match tokio::time::timeout(OUTPUT_GRACE, &mut h).await {
        Ok(r) => r.unwrap_or_default(),
        Err(_) => {
            h.abort();
            ("[output not collected: a process outside the command's group kept it open]".into(), true)
        }
    }
}

fn codes(status: Option<std::process::ExitStatus>) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Some(s) => (s.code(), s.signal()),
        None => (None, None),
    }
}

impl IntegrationService {
    pub async fn detail(&self, id: Uuid) -> Result<IntegrationDetail, ProtocolError> {
        self.inner
            .storage
            .call(move |c| {
                let i = store::get_integration(c, id)?;
                let plan = plan(c, &i)?;
                let runs = store::runs_for_integration(c, id)?
                    .into_iter()
                    .map(|r| run_wire(c, r))
                    .collect::<Result<_, _>>()?;
                Ok(IntegrationDetail { integration: wire_from_conn(c, i)?, runs, plan })
            })
            .await
            .map_err(errors::storage)
    }

    /// Starts a validation run for the integration's current source commit,
    /// as a Core-owned operation (ADR 0032) that lasts until the run ends.
    /// `authorize`: commands the policy asks about that the operator
    /// authorizes for this run, each with the exact command line shown. If
    /// any command is refused (denied, or asked about and not authorized),
    /// nothing runs. Returns once the run is recorded.
    pub async fn validate(
        &self,
        id: Uuid,
        authorize: Vec<switchyard_protocol::methods::ValidationAuthorization>,
        actor: String,
    ) -> Result<(Integration, ValidationRunInfo), ProtocolError> {
        let svc = self.clone();
        let by = actor.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let done = self
            .spawn_op(id, OpKind::Validate, actor, move |op| async move {
                svc.validate_work(op, id, authorize, by, started_tx).await
            })
            .await?;
        match started_rx.await {
            Ok(started) => started,
            // The work ended before a run started: its result says why.
            Err(_) => match done.await {
                Ok(Err(e)) => Err(e),
                _ => Err(ProtocolError::new(ErrorCode::Internal, "the validation stopped before it started")),
            },
        }
    }

    async fn validate_work(
        &self,
        op: Op,
        id: Uuid,
        authorize: Vec<switchyard_protocol::methods::ValidationAuthorization>,
        actor: String,
        started: oneshot::Sender<Result<(Integration, ValidationRunInfo), ProtocolError>>,
    ) -> Result<(), ProtocolError> {
        let i = self.prepare(id).await?;
        let blocking: Vec<String> = super::blockers_of(&i)
            .into_iter()
            .filter(|b| !VALIDATABLE.contains(&b.code.as_str()))
            .map(|b| b.message)
            .collect();
        if !blocking.is_empty() {
            return Err(invalid_state(format!("the source cannot be validated yet: {}", blocking.join(" "))));
        }
        let project_id = i.project_id;
        let w = self.workspace_of(&i).await?;
        let cwd = PathBuf::from(&w.path);
        let run_id = new_id();
        let source = i.source_commit.clone();

        // Lock the workspace first: no session may start while commands run.
        self.begin_workspace_step(op, w.id).await?;
        // One transaction: the commands, their policy decisions, the recorded
        // configuration and the steps that will run are the same snapshot.
        let created = {
            let (actor, cwd, source) = (actor.clone(), cwd.display().to_string(), source.clone());
            commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
                let now = now_ms();
                let mut drafts = Vec::new();
                let current = store::get_integration(tx, id)?;
                if current.source_commit != source {
                    return Ok(Err(ProtocolError::new(
                        ErrorCode::Conflict,
                        "the source commit changed meanwhile",
                    )));
                }
                let commands: Vec<CommandRecord> =
                    store::commands(tx, project_id)?.into_iter().filter(|c| c.enabled).collect();
                if commands.is_empty() {
                    return Ok(Err(invalid_state("no validation commands are configured for this project")));
                }
                let mut steps = Vec::new();
                for (pos, cmd) in commands.iter().enumerate() {
                    let e = evaluate_command(tx, project_id, current.workspace_id, &cmd.argv)?;
                    let line = command_line(&cmd.argv);
                    let (authorized_by, status) = match e.decision {
                        Decision::Allow => (Some("policy".to_owned()), StepStatus::Pending),
                        // Authorization names the exact command line the operator saw.
                        Decision::Ask
                            if authorize.iter().any(|a| a.command_id == cmd.id && a.command_line == line) =>
                        {
                            (Some(actor.clone()), StepStatus::Pending)
                        }
                        Decision::Ask | Decision::Deny => (None, StepStatus::Refused),
                    };
                    steps.push(StepRecord {
                        id: new_id(),
                        run_id,
                        position: pos as i64,
                        command_id: Some(cmd.id),
                        name: cmd.name.clone(),
                        argv: cmd.argv.clone(),
                        command_line: line,
                        cwd: cwd.clone(),
                        timeout_secs: cmd.timeout_secs,
                        policy_decision: e.decision.as_str().into(),
                        policy_reason: e.reason.as_str().into(),
                        policy_explanation: e.explanation.clone(),
                        policy_rule_id: e.deciding_rule.as_ref().map(|r| r.id),
                        policy_revision: e.policy_revision,
                        authorized_by,
                        status,
                        exit_code: None,
                        signal: None,
                        stdout: None,
                        stderr: None,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        detail: (status == StepStatus::Refused).then(|| match e.decision {
                            Decision::Deny => format!("denied by policy: {}", e.explanation),
                            _ => "the policy asks for your authorization, which this run did not include"
                                .into(),
                        }),
                        started_at_ms: None,
                        ended_at_ms: None,
                    });
                }
                store::insert_run(
                    tx,
                    &RunRecord {
                        id: run_id,
                        integration_id: id,
                        project_id,
                        config: config_of_steps(&steps),
                        source_commit: source.clone(),
                        status: RunStatus::Running,
                        requested_by: Some(actor.clone()),
                        started_at_ms: now,
                        ended_at_ms: None,
                    },
                )?;
                for step in &steps {
                    store::insert_step(tx, step)?;
                }
                drafts.push(draft(
                    DomainEvent::IntegrationValidationStarted {
                        integration_id: id,
                        run_id,
                        source_commit: source.clone(),
                        commands: steps.iter().map(|s| s.command_line.clone()).collect(),
                    },
                    &context(&current),
                )?);
                if steps.iter().any(|s| s.status == StepStatus::Refused) {
                    // Nothing runs: every command needs its authorization.
                    for s in &steps {
                        if s.status == StepStatus::Pending {
                            store::finish_step(
                                tx,
                                s.id,
                                StepStatus::Skipped,
                                &StepOutcome {
                                    detail: Some("not run: another command was refused".into()),
                                    ..Default::default()
                                },
                                now,
                            )?;
                        }
                    }
                    store::finish_run(tx, run_id, RunStatus::Failed, now)?;
                    drafts.push(draft(
                        DomainEvent::IntegrationValidationFinished {
                            integration_id: id,
                            run_id,
                            source_commit: source,
                            status: RunStatus::Failed.as_str().into(),
                        },
                        &context(&current),
                    )?);
                    return Ok(Ok((None, drafts)));
                }
                let u = IntegrationUpdate {
                    status: Some(IntegrationStatus::Validating),
                    status_reason: Some(Some(format!("validating {}", &source[..source.len().min(12)]))),
                    ..Default::default()
                };
                transition(tx, &current, u, now, &mut drafts)?;
                Ok(Ok((Some(steps), drafts)))
            })
            .await
        };
        let steps = match created {
            Ok(s) => s,
            Err(e) => {
                self.inner.workspaces.end_integration_op(w.id).await;
                return Err(e);
            }
        };
        let run_info = |svc: IntegrationService| async move {
            let run = svc
                .inner
                .storage
                .call(move |c| {
                    let r = store::get_run(c, run_id)?;
                    run_wire(c, r)
                })
                .await
                .map_err(errors::storage)?;
            Ok::<_, ProtocolError>((svc.wire(id).await?, run))
        };
        let Some(steps) = steps else {
            self.inner.workspaces.end_integration_op(w.id).await;
            self.prepare(id).await?;
            let _ = started.send(run_info(self.clone()).await);
            return Ok(());
        };
        let (tx, rx) = watch::channel(false);
        self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id, tx);
        if self.inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            // Shutdown began meanwhile: the run is stopped like any other.
            if let Some(c) =
                self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id)
            {
                let _ = c.send(true);
            }
        }
        let _ = started.send(run_info(self.clone()).await);
        self.execute_run(id, run_id, w.id, cwd, source, steps, rx).await;
        Ok(())
    }

    /// Runs exactly `steps` (the ones recorded with the run) in order. The
    /// run is `passed` only if there was at least one step and every step's
    /// success was both observed and recorded; any failure to record fails
    /// it. Always releases the workspace and settles the integration.
    #[allow(clippy::too_many_arguments)]
    async fn execute_run(
        &self,
        id: Uuid,
        run_id: Uuid,
        workspace_id: Uuid,
        cwd: PathBuf,
        source: String,
        steps: Vec<StepRecord>,
        mut cancel: watch::Receiver<bool>,
    ) {
        let git = self.inner.workspaces.git().clone();
        let expected = steps.len();
        let mut confirmed = 0usize;
        let mut run_status = RunStatus::Passed;
        let mut note: Option<String> = None;
        let mut stop = false;
        if expected == 0 {
            run_status = RunStatus::Failed;
            note = Some("The run had no commands to execute; it does not count.".into());
        }
        for step in steps {
            if stop {
                let sid = step.id;
                let _ = self
                    .inner
                    .storage
                    .call(move |c| {
                        store::finish_step(
                            c,
                            sid,
                            StepStatus::Skipped,
                            &StepOutcome {
                                detail: Some("not run: an earlier command did not pass".into()),
                                ..Default::default()
                            },
                            now_ms(),
                        )
                    })
                    .await;
                continue;
            }
            // The workspace must still be at the validated commit.
            let head = git.resolve_commit(&cwd, "HEAD").await.ok();
            let (status, mut outcome) = if head.as_deref() != Some(source.as_str()) {
                (
                    StepStatus::Error,
                    StepOutcome {
                        detail: Some("the workspace is no longer at the validated commit".into()),
                        ..Default::default()
                    },
                )
            } else {
                let sid = step.id;
                // A command whose start cannot be recorded does not run.
                match self.inner.storage.call(move |c| store::mark_step_running(c, sid, now_ms())).await {
                    Ok(true) => {
                        run_command(
                            &step.argv,
                            &cwd,
                            Duration::from_secs(u64::from(step.timeout_secs)),
                            &mut cancel,
                        )
                        .await
                    }
                    other => {
                        tracing::error!(%run_id, ?other, "cannot record the start of a validation step");
                        (
                            StepStatus::Error,
                            StepOutcome {
                                detail: Some("not run: its start could not be recorded".into()),
                                ..Default::default()
                            },
                        )
                    }
                }
            };
            if status == StepStatus::Cancelled
                && self.inner.stopping.load(std::sync::atomic::Ordering::SeqCst)
            {
                outcome.detail = Some("stopped: Core is shutting down".into());
            }
            if status != StepStatus::Passed {
                stop = true;
                run_status =
                    if status == StepStatus::Cancelled { RunStatus::Cancelled } else { RunStatus::Failed };
            }
            let (sid, name, line, decision, authorized_by) = (
                step.id,
                step.name.clone(),
                step.command_line.clone(),
                step.policy_decision.clone(),
                step.authorized_by.clone(),
            );
            let exit_code = outcome.exit_code;
            let recorded = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
                if !store::finish_step(tx, sid, status, &outcome, now_ms())? {
                    return Ok(Err(ProtocolError::new(ErrorCode::Internal, "the step was not running")));
                }
                let i = store::get_integration(tx, id)?;
                let d = draft(
                    DomainEvent::IntegrationValidationStepFinished {
                        integration_id: id,
                        run_id,
                        name,
                        command: line,
                        status: status.as_str().into(),
                        exit_code,
                        authorized_by,
                        policy_decision: decision,
                    },
                    &context(&i),
                )?;
                Ok(Ok(((), vec![d])))
            })
            .await;
            match recorded {
                Ok(()) if status == StepStatus::Passed => confirmed += 1,
                Ok(()) => {}
                Err(e) => {
                    tracing::error!(%run_id, ?e, "cannot record a validation step");
                    stop = true;
                    if run_status == RunStatus::Passed {
                        run_status = RunStatus::Failed;
                    }
                    note = Some("A command's result could not be recorded; the run does not count.".into());
                }
            }
        }
        let settled = final_run_status(expected, confirmed, run_status);
        if settled != run_status {
            run_status = settled;
            note = note
                .or_else(|| Some("Not every command's success was recorded; the run does not count.".into()));
        }
        // The commands must not have changed what was validated.
        if run_status == RunStatus::Passed {
            let head = git.resolve_commit(&cwd, "HEAD").await.ok();
            let changed = match (head.as_deref(), self.inner.workspaces.record(workspace_id).await) {
                (Some(h), Ok(w)) if h == source => {
                    let head_tree = git.tree_of(&cwd, "HEAD").await.ok();
                    let content = self.inner.workspaces.content_tree(&w).await.ok();
                    head_tree.is_none() || head_tree != content
                }
                _ => true,
            };
            if changed {
                run_status = RunStatus::Failed;
                note = Some(
                    "The validation commands changed the workspace (files that are not ignored, or the commit). \
                     Add build outputs to .gitignore or fix the commands; the run does not count."
                        .to_owned(),
                );
            }
        }
        self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
        if let Err(e) = self.end_run(id, run_id, &source, run_status).await {
            tracing::error!(%run_id, ?e, "cannot record the end of a validation run");
            // Never leave a pass that was not recorded as one: record a failure
            // instead, or leave the run `running` (it then counts for nothing,
            // and startup recovery marks it interrupted).
            if run_status != RunStatus::Failed
                && let Err(e) = self.end_run(id, run_id, &source, RunStatus::Failed).await
            {
                tracing::error!(%run_id, ?e, "cannot record the validation run as failed either");
            }
            note = Some("The validation result could not be recorded; the run does not count.".into());
        }
        self.inner.workspaces.end_integration_op(workspace_id).await;
        self.prepare_or_fail(id, note).await;
    }

    async fn end_run(
        &self,
        id: Uuid,
        run_id: Uuid,
        source: &str,
        status: RunStatus,
    ) -> Result<(), ProtocolError> {
        let source = source.to_owned();
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            if !store::finish_run(tx, run_id, status, now_ms())? {
                return Ok(Err(ProtocolError::new(ErrorCode::Internal, "the run was not running")));
            }
            let i = store::get_integration(tx, id)?;
            let d = draft(
                DomainEvent::IntegrationValidationFinished {
                    integration_id: id,
                    run_id,
                    source_commit: source,
                    status: status.as_str().into(),
                },
                &context(&i),
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await
    }

    /// Stops the running validation (its commands are terminated). This is
    /// the only explicit cancellation of a Core-owned operation.
    pub async fn cancel_validation(&self, id: Uuid) -> Result<Integration, ProtocolError> {
        let sender =
            self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id).cloned();
        match sender {
            Some(s) => {
                let _ = s.send(true);
            }
            None => return Err(invalid_state("no validation is running for this integration")),
        }
        // Give the runner a moment to record the cancellation.
        let deadline = tokio::time::Instant::now() + KILL_GRACE + OUTPUT_GRACE + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if self.record(id).await?.status != IntegrationStatus::Validating {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.wire(id).await
    }
}
