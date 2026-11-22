//! The workflow's test gate (ADR 0039).
//!
//! After an implementation step reports, Core runs the test commands the
//! implementer named, in the workflow's workspace, before anyone reviews the
//! work. Every command goes through the policy exactly like a validation
//! command (`allow` runs, `ask` needs the operator's resume, `deny` never
//! runs) and is run without a shell. Unit, integration and regression tests
//! are required; browser tests too when the change touches UI files (read
//! from Git, not from the agent). The run, its results and the workflow's
//! decision are committed in one transaction.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use switchyard_events::{new_id, now_ms};
use switchyard_integrations::{StepStatus, command_line};
use switchyard_policy::Decision;
use switchyard_protocol::ProtocolError;
use switchyard_storage::workflows::{self as wf_store, TestRunRecord};
use switchyard_storage::{Connection, StorageError, tasks as store};
use switchyard_tasks::context::{ContextTestResult, ContextTests};
use switchyard_tasks::report::{TestCommand, TestKind};
use switchyard_tasks::workflow::{Input, Tested, WorkflowState};
use uuid::Uuid;

use super::TaskService;
use crate::handlers::errors;
use crate::integrations::validate::{evaluate_command, run_command};
use crate::journal::commit_checked;

const DEFAULT_TIMEOUT_SECS: u32 = 900;
const MAX_TIMEOUT_SECS: u32 = 3600;
/// Output kept per stream in the record.
const TAIL_BYTES: usize = 8 * 1024;
/// Output passed on to the next agent per command.
const CONTEXT_TAIL: usize = 2000;

/// File extensions that make a change a UI change (browser tests required).
const UI_EXTENSIONS: &[&str] = &[
    "tsx", "jsx", "vue", "svelte", "astro", "html", "htm", "css", "scss", "sass", "less", "erb", "hbs",
    "ejs", "twig", "liquid",
];

/// Whether any changed path is a UI file.
pub(crate) fn is_ui_change(paths: &[String]) -> bool {
    paths.iter().any(|p| {
        p.rsplit_once('.').is_some_and(|(_, ext)| UI_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
    })
}

/// The kinds a change must be tested with.
pub(crate) fn required_kinds(ui_change: bool) -> Vec<TestKind> {
    let mut k = TestKind::REQUIRED.to_vec();
    if ui_change {
        k.push(TestKind::Browser);
    }
    k
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

/// Core's latest test run of the task, for the next agent's context.
pub(crate) fn latest_for_context(c: &Connection, task: Uuid) -> Result<Option<ContextTests>, StorageError> {
    let Some(run) = wf_store::test_runs_for_task(c, task)?.pop() else { return Ok(None) };
    let results = run
        .results
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| ContextTestResult {
                    kind: serde_json::from_value(r["kind"].clone()).unwrap_or(TestKind::Unit),
                    command: r["command"].as_str().unwrap_or_default().to_owned(),
                    status: r["status"].as_str().unwrap_or("unknown").to_owned(),
                    exit_code: r["exit_code"].as_i64().and_then(|c| i32::try_from(c).ok()),
                    output_tail: tail(
                        &format!(
                            "{}{}",
                            r["stdout_tail"].as_str().unwrap_or_default(),
                            r["stderr_tail"].as_str().unwrap_or_default()
                        ),
                        CONTEXT_TAIL,
                    ),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(ContextTests { status: run.status, summary: run.summary, results }))
}

/// The test commands the step's implementation report named, from Core's
/// record of that report.
fn named_tests(c: &Connection, task: Uuid, step_key: &str) -> Result<Vec<TestCommand>, StorageError> {
    let Some(a) =
        store::assignments_for_task(c, task)?.into_iter().find(|a| a.step_key.as_deref() == Some(step_key))
    else {
        return Ok(Vec::new());
    };
    for m in store::messages_for_task(c, task)?.into_iter().rev() {
        if m.assignment_id == Some(a.id) && m.payload["report"]["type"] == "implementation" {
            return Ok(serde_json::from_value(m.payload["report"]["tests"].clone()).unwrap_or_default());
        }
    }
    Ok(Vec::new())
}

struct Plan {
    project_id: Uuid,
    workspace_id: Uuid,
    workspace_path: PathBuf,
    base_commit: Option<String>,
    step_key: String,
    tests: Vec<TestCommand>,
}

impl TaskService {
    /// Runs the test gate for the workflow's current implementation step and
    /// commits its result with the workflow's decision. `authorized` is the
    /// operator's resume: it authorizes commands the policy asks about.
    pub(super) async fn run_tests(&self, task_id: Uuid, authorized: bool, by: String) {
        if let Err(e) = self.run_tests_once(task_id, authorized, by).await {
            tracing::warn!(%task_id, ?e, "the workflow's test gate could not run");
            let step = self
                .storage
                .call(move |c| {
                    let w = wf_store::get_workflow(c, task_id)?;
                    let state: WorkflowState = serde_json::from_value(w.state.clone()).unwrap_or_default();
                    Ok(state.current.map(|s| s.key()))
                })
                .await
                .ok()
                .flatten();
            if let Some(step) = step {
                let result = Tested::Failed(format!("the tests could not run: {}", e.message));
                if let Err(e) = self.record_tests(task_id, step, result, None).await {
                    tracing::error!(%task_id, ?e, "cannot record the failed test gate");
                }
            }
        }
    }

    async fn run_tests_once(&self, task_id: Uuid, authorized: bool, by: String) -> Result<(), ProtocolError> {
        let plan = self
            .storage
            .call(move |c| {
                let w = wf_store::get_workflow(c, task_id)?;
                let state: WorkflowState = serde_json::from_value(w.state.clone())
                    .map_err(|e| StorageError::Corrupt(format!("workflow state: {e}")))?;
                let step_key = state.current.map(|s| s.key()).unwrap_or_default();
                let workspace_id = w
                    .workspace_id
                    .ok_or_else(|| StorageError::Corrupt("workflow without a workspace".into()))?;
                let ws = switchyard_storage::workspaces::get(c, workspace_id)?;
                Ok(Plan {
                    project_id: w.project_id,
                    workspace_id,
                    workspace_path: PathBuf::from(&ws.path),
                    base_commit: ws.base_commit.clone(),
                    tests: named_tests(c, task_id, &step_key)?,
                    step_key,
                })
            })
            .await
            .map_err(errors::storage)?;
        let started = now_ms();

        // UI change: read from Git, never from the agent.
        let ws = self.workspaces.record(plan.workspace_id).await?;
        let changed = match &plan.base_commit {
            Some(base) => self.workspaces.changed_files(&ws, base).await.unwrap_or_default(),
            None => Vec::new(),
        };
        let ui_change = is_ui_change(&changed);
        let present: BTreeSet<TestKind> = plan.tests.iter().map(|t| t.kind).collect();
        let missing: Vec<&str> = required_kinds(ui_change)
            .into_iter()
            .filter(|k| !present.contains(k))
            .map(TestKind::as_str)
            .collect();

        // The policy's answer for every command, read in one place.
        let (project, workspace) = (plan.project_id, plan.workspace_id);
        let argvs: Vec<Vec<String>> = plan.tests.iter().map(|t| t.command.clone()).collect();
        let decisions: Vec<(Decision, String)> = self
            .storage
            .call(move |c| {
                argvs
                    .iter()
                    .map(|argv| {
                        evaluate_command(c, project, workspace, argv).map(|e| (e.decision, e.explanation))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(errors::storage)?;

        let mut results: Vec<Value> = Vec::new();
        let describe = |t: &TestCommand, policy: &str, status: &str, detail: Option<String>| {
            json!({
                "kind": t.kind, "name": t.name, "command": command_line(&t.command), "policy": policy,
                "status": status, "detail": detail,
            })
        };
        let denied: Vec<String> = plan
            .tests
            .iter()
            .zip(&decisions)
            .filter(|(_, (d, _))| *d == Decision::Deny)
            .map(|(t, _)| command_line(&t.command))
            .collect();
        let asking: Vec<String> = plan
            .tests
            .iter()
            .zip(&decisions)
            .filter(|(_, (d, _))| *d == Decision::Ask)
            .map(|(t, _)| command_line(&t.command))
            .collect();

        let result = if !missing.is_empty() || !denied.is_empty() {
            for (t, (d, why)) in plan.tests.iter().zip(&decisions) {
                let status = if *d == Decision::Deny { "refused" } else { "skipped" };
                results.push(describe(t, d.as_str(), status, Some(why.clone())));
            }
            let mut why = Vec::new();
            if !missing.is_empty() {
                why.push(format!(
                    "no {} tests were named{}",
                    missing.join(", "),
                    if ui_change && missing.contains(&"browser") {
                        " (the change touches UI files)"
                    } else {
                        ""
                    }
                ));
            }
            if !denied.is_empty() {
                why.push(format!("blocked by your policies: {}", denied.join("; ")));
            }
            Tested::Failed(why.join("; "))
        } else if !asking.is_empty() && !authorized {
            for (t, (d, why)) in plan.tests.iter().zip(&decisions) {
                results.push(describe(t, d.as_str(), "skipped", Some(why.clone())));
            }
            Tested::NeedsAuthorization(asking.join("; "))
        } else {
            let (_keep, mut cancel) = tokio::sync::watch::channel(false);
            let mut failed = Vec::new();
            for (t, (d, _)) in plan.tests.iter().zip(&decisions) {
                let timeout = t.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).clamp(1, MAX_TIMEOUT_SECS);
                let clock = Instant::now();
                let (status, out) = run_command(
                    &t.command,
                    &plan.workspace_path,
                    Duration::from_secs(u64::from(timeout)),
                    &mut cancel,
                )
                .await;
                if status != StepStatus::Passed {
                    failed.push(format!("{} ({})", command_line(&t.command), status.as_str()));
                }
                results.push(json!({
                    "kind": t.kind, "name": t.name, "command": command_line(&t.command), "policy": d.as_str(),
                    "status": status.as_str(), "exit_code": out.exit_code, "signal": out.signal,
                    "duration_ms": u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "stdout_tail": tail(out.stdout.as_deref().unwrap_or_default(), TAIL_BYTES),
                    "stderr_tail": tail(out.stderr.as_deref().unwrap_or_default(), TAIL_BYTES),
                    "detail": out.detail,
                }));
            }
            if failed.is_empty() {
                Tested::Passed(format!("{} test commands passed", plan.tests.len()))
            } else {
                Tested::Failed(format!("failed: {}", failed.join("; ")))
            }
        };
        let run = TestRunRecord {
            id: new_id(),
            task_id,
            step_key: plan.step_key.clone(),
            status: match &result {
                Tested::Passed(_) => "passed",
                Tested::Failed(_) => "failed",
                Tested::NeedsAuthorization(_) => "needs_authorization",
            }
            .into(),
            summary: match &result {
                Tested::Passed(s) | Tested::Failed(s) | Tested::NeedsAuthorization(s) => s.clone(),
            },
            results: Value::Array(results),
            ui_change,
            authorized_by: (authorized && !asking.is_empty()).then_some(by),
            started_at_ms: started,
            ended_at_ms: now_ms(),
        };
        self.record_tests(task_id, plan.step_key, result, Some(run)).await
    }

    /// Commits the run (if any) together with the workflow's decision on it.
    async fn record_tests(
        &self,
        task_id: Uuid,
        step: String,
        result: Tested,
        run: Option<TestRunRecord>,
    ) -> Result<(), ProtocolError> {
        let lock = self.workflow_lock(task_id);
        let guard = lock.lock().await;
        let input = Input::Tested { step, result };
        let followup = commit_checked(&self.storage, &self.bus, move |tx| {
            let mut drafts = Vec::new();
            if let Some(run) = &run {
                wf_store::insert_test_run(tx, run)?;
            }
            let f = super::check!(super::workflow::decide_in_tx(
                tx,
                task_id,
                &input,
                "core",
                now_ms(),
                &mut drafts
            )?);
            Ok(Ok((f, drafts)))
        })
        .await?;
        drop(guard);
        if let Some(f) = followup {
            self.perform(task_id, f);
        }
        Ok(())
    }
}

#[cfg(test)]
mod unit {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ui_changes_are_detected_from_file_extensions() {
        assert!(is_ui_change(&["src/App.tsx".into()]));
        assert!(is_ui_change(&["a/b/style.SCSS".into()]));
        assert!(!is_ui_change(&["src/lib.rs".into(), "README.md".into(), "Makefile".into()]));
        assert_eq!(required_kinds(false), TestKind::REQUIRED.to_vec());
        assert!(required_kinds(true).contains(&TestKind::Browser));
    }

    #[test]
    fn tails_keep_the_end_on_a_char_boundary() {
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("ääää", 3), "…ä");
    }
}
