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
use switchyard_integrations::{IntegrationStatus, MAX_OUTPUT_BYTES, RunStatus, StepStatus, command_line};
use switchyard_policy::{Decision, EvaluationInput, Operation, OperationCategory, RequestScope, evaluate};
use switchyard_protocol::methods::IntegrationDetail;
use switchyard_protocol::model::{Integration, ValidationPlanEntry, ValidationRunInfo, ValidationStepInfo};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{
    self as store, CommandRecord, IntegrationRecord, IntegrationUpdate, RunRecord, StepOutcome, StepRecord,
};
use switchyard_storage::{Connection, StorageError, policies};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;
use uuid::Uuid;

use super::{IntegrationService, context, invalid_state, transition, wire_from_conn};
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

/// What a validation run is valid for besides the source commit: the enabled
/// commands (exact command lines and timeouts) and the policy's decision for
/// each. A passed run counts only while this is unchanged.
pub(super) fn validation_config(
    c: &Connection,
    project_id: Uuid,
    workspace_id: Uuid,
) -> Result<String, StorageError> {
    let mut lines = Vec::new();
    for cmd in store::commands(c, project_id)?.into_iter().filter(|cmd| cmd.enabled) {
        let e = evaluate_command(c, project_id, workspace_id, &cmd.argv)?;
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            cmd.id,
            command_line(&cmd.argv),
            cmd.timeout_secs,
            e.decision.as_str()
        ));
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

    /// Starts a validation run for the integration's current source commit.
    /// `authorize`: commands the policy asks about that the operator
    /// authorizes for this run. If any command is refused (denied, or asked
    /// about and not authorized), nothing runs.
    pub async fn validate(
        &self,
        id: Uuid,
        authorize: Vec<switchyard_protocol::methods::ValidationAuthorization>,
        actor: String,
    ) -> Result<(Integration, ValidationRunInfo), ProtocolError> {
        let claim = self.claim(id)?;
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
        let commands: Vec<CommandRecord> = self
            .inner
            .storage
            .call(move |c| Ok(store::commands(c, project_id)?.into_iter().filter(|c| c.enabled).collect()))
            .await
            .map_err(errors::storage)?;
        if commands.is_empty() {
            return Err(invalid_state("no validation commands are configured for this project"));
        }
        let w = self.workspace_of(&i).await?;
        let cwd = PathBuf::from(&w.path);
        let run_id = new_id();
        let source = i.source_commit.clone();

        // Lock the workspace first: no session may start while commands run.
        self.inner.workspaces.begin_integration_op(w.id).await.map_err(|e| {
            if e.code == ErrorCode::WorkspaceOccupied {
                invalid_state("an agent session is running in the workspace; stop it first")
            } else {
                e
            }
        })?;
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
                let config = validation_config(tx, project_id, current.workspace_id)?;
                store::insert_run(
                    tx,
                    &RunRecord {
                        id: run_id,
                        integration_id: id,
                        project_id,
                        config,
                        source_commit: source.clone(),
                        status: RunStatus::Running,
                        requested_by: Some(actor.clone()),
                        started_at_ms: now,
                        ended_at_ms: None,
                    },
                )?;
                let mut refused = 0;
                for (pos, cmd) in commands.iter().enumerate() {
                    let e = evaluate_command(tx, project_id, current.workspace_id, &cmd.argv)?;
                    let (authorized_by, status) = match e.decision {
                        Decision::Allow => (Some("policy".to_owned()), StepStatus::Pending),
                        // Authorization names the exact command line the operator saw.
                        Decision::Ask
                            if authorize.iter().any(|a| {
                                a.command_id == cmd.id && a.command_line == command_line(&cmd.argv)
                            }) =>
                        {
                            (Some(actor.clone()), StepStatus::Pending)
                        }
                        Decision::Ask | Decision::Deny => (None, StepStatus::Refused),
                    };
                    if status == StepStatus::Refused {
                        refused += 1;
                    }
                    store::insert_step(
                        tx,
                        &StepRecord {
                            id: new_id(),
                            run_id,
                            position: pos as i64,
                            command_id: Some(cmd.id),
                            name: cmd.name.clone(),
                            argv: cmd.argv.clone(),
                            command_line: command_line(&cmd.argv),
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
                        },
                    )?;
                }
                drafts.push(draft(
                    DomainEvent::IntegrationValidationStarted {
                        integration_id: id,
                        run_id,
                        source_commit: source.clone(),
                        commands: commands.iter().map(|c| command_line(&c.argv)).collect(),
                    },
                    &context(&current),
                )?);
                if refused > 0 {
                    // Nothing runs: every command needs its authorization.
                    for s in store::steps_for_run(tx, run_id)? {
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
                    return Ok(Ok((false, drafts)));
                }
                let u = IntegrationUpdate {
                    status: Some(IntegrationStatus::Validating),
                    status_reason: Some(Some(format!("validating {}", &source[..source.len().min(12)]))),
                    ..Default::default()
                };
                transition(tx, &current, u, now, &mut drafts)?;
                Ok(Ok((true, drafts)))
            })
            .await
        };
        let started = match created {
            Ok(s) => s,
            Err(e) => {
                self.inner.workspaces.end_integration_op(w.id).await;
                return Err(e);
            }
        };
        if !started {
            self.inner.workspaces.end_integration_op(w.id).await;
            self.prepare(id).await?;
        } else {
            let (tx, rx) = watch::channel(false);
            self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id, tx);
            let svc = self.clone();
            let workspace_id = w.id;
            tokio::spawn(async move {
                let _claim = claim;
                svc.execute_run(id, run_id, workspace_id, cwd, source, rx).await;
            });
        }
        let run = self
            .inner
            .storage
            .call(move |c| {
                let r = store::get_run(c, run_id)?;
                run_wire(c, r)
            })
            .await
            .map_err(errors::storage)?;
        Ok((self.wire(id).await?, run))
    }

    async fn execute_run(
        &self,
        id: Uuid,
        run_id: Uuid,
        workspace_id: Uuid,
        cwd: PathBuf,
        source: String,
        mut cancel: watch::Receiver<bool>,
    ) {
        let git = self.inner.workspaces.git().clone();
        let steps = match self.inner.storage.call(move |c| store::steps_for_run(c, run_id)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(%run_id, ?e, "cannot read validation steps");
                Vec::new()
            }
        };
        let mut run_status = RunStatus::Passed;
        let mut stop = false;
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
            let (status, outcome) = if head.as_deref() != Some(source.as_str()) {
                (
                    StepStatus::Error,
                    StepOutcome {
                        detail: Some("the workspace is no longer at the validated commit".into()),
                        ..Default::default()
                    },
                )
            } else {
                let sid = step.id;
                let _ = self.inner.storage.call(move |c| store::mark_step_running(c, sid, now_ms())).await;
                run_command(&step.argv, &cwd, Duration::from_secs(u64::from(step.timeout_secs)), &mut cancel)
                    .await
            };
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
            let r = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
                store::finish_step(tx, sid, status, &outcome, now_ms())?;
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
            if let Err(e) = r {
                tracing::error!(%run_id, ?e, "cannot record a validation step");
            }
        }
        // The commands must not have changed what was validated.
        let mut note = None;
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
        let note_for_reason = note.clone();
        let finished = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            store::finish_run(tx, run_id, run_status, now)?;
            let i = store::get_integration(tx, id)?;
            let drafts = vec![draft(
                DomainEvent::IntegrationValidationFinished {
                    integration_id: id,
                    run_id,
                    source_commit: source,
                    status: run_status.as_str().into(),
                },
                &context(&i),
            )?];
            let _ = (&i, now, IntegrationUpdate::default());
            Ok(Ok(((), drafts)))
        })
        .await;
        if let Err(e) = finished {
            tracing::error!(%run_id, ?e, "cannot record the end of a validation run");
        }
        self.inner.workspaces.end_integration_op(workspace_id).await;
        if let Err(e) = self.prepare_with(id, note_for_reason).await {
            tracing::error!(integration = %id, ?e, "cannot prepare the integration after validation");
        }
    }

    /// Stops the running validation (its commands are terminated).
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
        let deadline = tokio::time::Instant::now() + KILL_GRACE + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if self.record(id).await?.status != IntegrationStatus::Validating {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.wire(id).await
    }
}
