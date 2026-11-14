//! Preparation: everything an integration is checked against, read from Git
//! and from Core's own records. Nothing here comes from an agent's report.

use std::path::Path;

use serde_json::{Value, json};
use switchyard_events::DomainEvent;
use switchyard_git::MergeOutcome;
use switchyard_integrations::{
    Blocker, Facts, MergeFact, ReviewFact, RunStatus, TargetCheckoutFact, blockers,
};
use switchyard_protocol::ProtocolError;
use switchyard_storage::integrations as store;
use switchyard_storage::tasks as task_store;
use switchyard_storage::{WorkspaceRecord, workspaces};
use switchyard_tasks::{AssignmentStatus, Role, TaskStatus};
use switchyard_workspaces::WorkspaceStatus;
use uuid::Uuid;

use super::{IntegrationService, git_err, invalid_params};
use crate::handlers::errors;

/// The result of one preparation.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub source_commit: String,
    pub target_commit: String,
    pub base_commit: Option<String>,
    pub facts: Facts,
    pub blockers: Vec<Blocker>,
    pub files_changed: u64,
    /// A merge (update from the target) is in progress in the workspace.
    pub workspace_merge_in_progress: bool,
    /// Stored with the integration: facts, blockers, summary.
    pub preflight: Value,
}

impl Prepared {
    pub fn event(&self, integration_id: Uuid) -> DomainEvent {
        DomainEvent::IntegrationPrepared {
            integration_id,
            source_commit: self.source_commit.clone(),
            target_commit: self.target_commit.clone(),
            base_commit: self.base_commit.clone(),
            blockers: self.blockers.iter().map(|b| b.code.clone()).collect(),
            files_changed: self.files_changed,
        }
    }
}

/// What the task and its latest review say, from Core's records.
struct TaskFacts {
    task: Option<Result<(), String>>,
    /// The latest review: (verdict outcome, reviewed tree, its workspace).
    review: Option<(Option<String>, Option<String>, Uuid)>,
}

impl IntegrationService {
    /// Reads every fact about delivering `w` into `target_branch`.
    pub(crate) async fn gather(
        &self,
        w: &WorkspaceRecord,
        target_branch: &str,
        task_id: Option<Uuid>,
        integration_id: Option<Uuid>,
    ) -> Result<Prepared, ProtocolError> {
        let git = self.inner.workspaces.git().clone();
        let root = self.inner.workspaces.project_root(w.project_id).await?;
        let ws_path = Path::new(&w.path);
        let source_branch = w.branch.clone().ok_or_else(|| invalid_params("the workspace has no branch"))?;

        // Commits: the source branch tip and the target branch tip.
        let source_commit =
            git.branch_tip(&root, &source_branch).await.map_err(git_err)?.ok_or_else(|| {
                invalid_params(format!("the workspace branch {source_branch} does not exist"))
            })?;
        let target_commit = git
            .branch_tip(&root, target_branch)
            .await
            .map_err(git_err)?
            .ok_or_else(|| invalid_params(format!("the target branch {target_branch} does not exist")))?;

        // The workspace itself.
        let inspection = self.inner.workspaces.inspect(w, &root).await;
        let usable = matches!(
            w.status,
            WorkspaceStatus::Ready
                | WorkspaceStatus::Dirty
                | WorkspaceStatus::Completed
                | WorkspaceStatus::Active
                | WorkspaceStatus::Integrating
        );
        let workspace_valid = match (&inspection.problem, usable) {
            (Some(p), _) => Err(p.reason()),
            (None, false) => Err(format!("the workspace is {}", w.status)),
            (None, true) => Ok(()),
        };
        let wid = w.id;
        let live_session = self
            .inner
            .storage
            .call(move |c| workspaces::live_session(c, wid))
            .await
            .map_err(errors::storage)?
            .is_some();
        let (uncommitted_changes, state) = if workspace_valid.is_ok() {
            let state = git.repo_state(ws_path).await.map_err(git_err)?;
            let head_tree = git.tree_of(ws_path, "HEAD").await.map_err(git_err)?;
            let content = self.inner.workspaces.content_tree(w).await.map_err(git_err)?;
            (content != head_tree || state.merge_in_progress, state)
        } else {
            (false, switchyard_git::RepoState::default())
        };

        // Source versus target.
        let source_tree = git.tree_of(&root, &source_commit).await.map_err(git_err)?;
        let target_tree = git.tree_of(&root, &target_commit).await.map_err(git_err)?;
        let base_commit = git.merge_base(&root, &target_commit, &source_commit).await.map_err(git_err)?;
        let source_ahead = git.count_commits(&root, &target_commit, &source_commit).await.map_err(git_err)?;
        let merge = if git.is_ancestor(&root, &target_commit, &source_commit).await.map_err(git_err)? {
            MergeFact::UpToDate
        } else {
            match git.merge_tree(&root, &target_commit, &source_commit).await.map_err(git_err)? {
                MergeOutcome::Clean { .. } => MergeFact::Clean,
                MergeOutcome::Conflicts { files, .. } => MergeFact::Conflicts { files },
            }
        };
        let from = base_commit.clone().unwrap_or_else(|| target_commit.clone());
        let summary = git.diff_summary(&root, &from, &source_commit, 0).await.map_err(git_err)?;

        // Where the target branch is checked out (usually the project checkout).
        let wanted = format!("refs/heads/{target_branch}");
        let checkout = git
            .list_worktrees(&root)
            .await
            .map_err(git_err)?
            .into_iter()
            .find(|e| e.branch.as_deref() == Some(wanted.as_str()));
        let target_checkout = match checkout {
            Some(e) => {
                let st = git.status(&e.path).await.map_err(git_err)?;
                let busy = git.repo_state(&e.path).await.map_err(git_err)?;
                Some(TargetCheckoutFact {
                    path: e.path.display().to_string(),
                    has_local_changes: st.staged + st.unstaged + st.conflicted > 0,
                    busy: busy.describe().into_iter().map(str::to_owned).collect(),
                })
            }
            None => None,
        };
        let identity = git.committer_ident(&root).await.map_err(|e| e.to_string());

        // Task, review, settings and validation, from Core's records.
        let project_id = w.project_id;
        let (settings, commands, runs, task_facts, config) = self
            .inner
            .storage
            .call(move |c| {
                let settings = store::settings(c, project_id)?;
                let config = super::validate::validation_config(c, project_id, wid)?;
                let commands = store::commands(c, project_id)?.into_iter().filter(|c| c.enabled).count() as u32;
                let runs = match integration_id {
                    Some(id) => store::runs_for_integration(c, id)?,
                    None => Vec::new(),
                };
                let task_facts = match task_id {
                    None => TaskFacts { task: None, review: None },
                    Some(t) => {
                        let task = task_store::get_task(c, t)?;
                        let assignments = task_store::assignments_for_task(c, t)?;
                        let working = assignments.iter().any(|a| a.status.is_open());
                        let ok = match task.status {
                            _ if working => Err("An agent is still working on the task.".to_owned()),
                            TaskStatus::AwaitingReview | TaskStatus::Completed => Ok(()),
                            s => Err(format!(
                                "The task is {s}; integrate work that was reported (awaiting review) or completed."
                            )),
                        };
                        let review = assignments
                            .iter()
                            .rev()
                            .find(|a| a.role == Role::Review && a.status == AssignmentStatus::Reported)
                            .map(|a| -> Result<_, switchyard_storage::StorageError> {
                                Ok((a.outcome.clone(), store::review_tree(c, a.id)?, a.workspace_id))
                            })
                            .transpose()?;
                        TaskFacts { task: Some(ok), review }
                    }
                };
                Ok((settings, commands, runs, task_facts, config))
            })
            .await
            .map_err(errors::storage)?;

        let review = review_fact(
            settings.require_review,
            task_id.is_some(),
            &task_facts,
            &source_tree,
            w.id,
            uncommitted_changes,
        );
        // A run counts only for this exact commit and the current command
        // configuration and policy decisions.
        let latest = runs.iter().find(|r| r.source_commit == source_commit && r.config == config);
        let validation_passed = latest.is_some_and(|r| r.status == RunStatus::Passed);
        let validation_failed = latest.is_some_and(|r| {
            matches!(r.status, RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted)
        });

        let facts = Facts {
            workspace_valid,
            live_session,
            uncommitted_changes,
            workspace_busy: state
                .describe()
                .into_iter()
                .filter(|d| !d.starts_with("a merge"))
                .map(str::to_owned)
                .collect(),
            source_ahead,
            has_changes: source_tree != target_tree,
            merge,
            target_checkout,
            identity,
            task: task_facts.task,
            review,
            validations_configured: commands,
            validation_passed,
            validation_failed,
        };
        let found = blockers(&facts);
        let preflight = json!({
            "facts": facts,
            "blockers": found,
            "summary": {
                "commits_ahead": source_ahead,
                "files_changed": summary.total_files,
                "additions": summary.additions,
                "deletions": summary.deletions,
                "source_tree": source_tree,
            },
            "require_review": settings.require_review,
        });
        Ok(Prepared {
            source_commit,
            target_commit,
            base_commit,
            files_changed: summary.total_files,
            blockers: found,
            facts,
            workspace_merge_in_progress: state.merge_in_progress,
            preflight,
        })
    }
}

fn review_fact(
    required: bool,
    has_task: bool,
    t: &TaskFacts,
    source_tree: &str,
    workspace: Uuid,
    uncommitted: bool,
) -> ReviewFact {
    let fail = |detail: &str| ReviewFact { required, satisfied: false, detail: detail.to_owned() };
    if !has_task {
        return fail(
            "The integration has no task, so no review can be checked. Review is required in this project's \
             integration settings.",
        );
    }
    let Some((outcome, tree, review_ws)) = &t.review else {
        return fail("No review of this task has been reported. Assign a review first.");
    };
    if outcome.as_deref() != Some("review: approved") {
        return fail(&format!(
            "The latest review did not approve ({}). Address it and review again.",
            outcome.as_deref().unwrap_or("no verdict")
        ));
    }
    if *review_ws != workspace {
        return fail("The latest approved review looked at a different workspace.");
    }
    match tree {
        Some(t) if t == source_tree => ReviewFact {
            required,
            satisfied: true,
            detail: "The latest review approved exactly this content.".into(),
        },
        Some(_) if uncommitted => fail(
            "The source commit does not contain the content the latest review approved: commit the workspace (or, if \
             it changed after the review, assign a new review).",
        ),
        Some(_) => fail(
            "The source commit differs from the content the latest review approved (the workspace changed after the \
             review). Assign a new review of the current content.",
        ),
        None => fail(
            "The latest review did not record which content it saw (it predates change integration). Assign a new \
             review.",
        ),
    }
}
