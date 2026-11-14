//! Approval and integration (ADRs 0028, 0031, 0032).
//!
//! Approval runs as a Core-owned `approve` operation: once the operator has
//! approved, the work below runs to its end even if the client disconnects.
//!
//! 1. Under the project's repository lock, Core re-reads every fact and
//!    checks that the source and target are still exactly the commits the
//!    operator approved, and that nothing blocks.
//! 2. It computes the merge in memory and creates the merge commit object
//!    (parents: target, source). Nothing has moved yet.
//! 3. It records `integrating` with that merge commit, durably.
//! 4. It moves the target branch: one compare-and-swap ref update, or, if
//!    the target is checked out, a fast-forward of that clean checkout bound
//!    to the target branch at the approved commit.
//! 5. It records `integrated`, or `failed` with what it then observes in
//!    Git. It says a checkout is unchanged only when it checked that.
//!
//! After a crash between 3 and 5, recovery compares the target branch with
//! the recorded merge commit (see [`IntegrationService::recover_on_startup`]).

use std::path::{Path, PathBuf};

use switchyard_events::{DomainEvent, now_ms};
use switchyard_git::{FastForwardError, Git, GitError, MergeOutcome};
use switchyard_integrations::{IntegrationStatus, merge_message};
use switchyard_protocol::model::Integration;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{self as store, IntegrationRecord, IntegrationUpdate};
use switchyard_storage::tasks as task_store;
use uuid::Uuid;

use super::{
    Ending, IntegrationService, Op, OpKind, blockers_of, context, git_err, invalid_state, transition,
};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

/// How an integration ended: `Ok(note)` integrated, `Err((reason, target_moved))` failed.
type Outcome = Result<Option<String>, (String, bool)>;

const METHOD_CHECKOUT: &str = "checkout_fast_forward";

/// Where `branch` is checked out, if anywhere.
async fn checkout_of(git: &Git, root: &Path, branch: &str) -> Result<Option<PathBuf>, GitError> {
    let wanted = format!("refs/heads/{branch}");
    Ok(git
        .list_worktrees(root)
        .await?
        .into_iter()
        .find(|e| e.branch.as_deref() == Some(&wanted))
        .map(|e| e.path))
}

/// The failure of a fast-forward of the checkout at `path`, described from
/// what Git shows now. "Nothing was changed" only if that was checked.
async fn describe_checkout_failure(
    git: &Git,
    root: &Path,
    path: &Path,
    i: &IntegrationRecord,
    merge: &str,
    what: &str,
    tip: Option<&str>,
) -> String {
    let unchanged = match git.changed_paths(root, &i.target_commit, merge).await {
        Ok(paths) => {
            git.checkout_unchanged(path, &i.target_branch, &i.target_commit, &paths).await.unwrap_or(false)
        }
        Err(_) => false,
    };
    let branch_state = match tip {
        Some(t) if t == i.target_commit => format!("{} was not moved", i.target_branch),
        Some(t) => format!("{} is now at {t}", i.target_branch),
        None => format!("{} does not exist any more", i.target_branch),
    };
    if unchanged {
        return format!(
            "{what}. Checked afterwards: the checkout at {} is still on {} at {} with no changes, so nothing was \
             changed; {branch_state}.",
            path.display(),
            i.target_branch,
            i.target_commit
        );
    }
    let lock = match git.repo_state(path).await {
        Ok(st) if st.index_locked => {
            " Its index.lock file is still present: remove it only after making sure no Git process is running in \
             that repository."
        }
        _ => "",
    };
    format!(
        "{what}. The checkout at {} may be partially updated: inspect it with `git status` before doing anything \
         else there. Switchyard did not undo or repair anything; {branch_state}.{lock}",
        path.display()
    )
}

impl IntegrationService {
    /// The operator approves delivering exactly `source_commit` into the
    /// target branch at exactly `target_commit`. Only this method moves a
    /// target branch, and only Core's operator IPC reaches it. The work runs
    /// as a Core-owned operation (ADR 0032).
    pub async fn approve(
        &self,
        id: Uuid,
        source_commit: String,
        target_commit: String,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let i = self.record(id).await?;
        if i.status == IntegrationStatus::Integrated {
            return Err(ProtocolError::new(ErrorCode::Conflict, "the integration is already integrated"));
        }
        if i.status != IntegrationStatus::Ready {
            return Err(invalid_state(format!("the integration is {}, not ready for approval", i.status)));
        }
        if i.source_commit != source_commit || i.target_commit != target_commit {
            return Err(stale("the approval names other commits than the integration is bound to", &i));
        }
        let svc = self.clone();
        let by = actor.clone();
        self.run_op(id, OpKind::Approve, actor, move |op| async move {
            svc.approve_work(op, id, source_commit, target_commit, by).await
        })
        .await
    }

    async fn approve_work(
        &self,
        op: Op,
        id: Uuid,
        source_commit: String,
        target_commit: String,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let i = self.record(id).await?;
        if i.status != IntegrationStatus::Ready {
            return Err(invalid_state(format!("the integration is {}, not ready for approval", i.status)));
        }
        let lock = self.inner.workspaces.repo_lock(i.project_id);
        let _repo = lock.lock().await;

        // 1. Re-check everything now, under the lock.
        let w = self.inner.workspaces.record(i.workspace_id).await?;
        let prepared = self.gather(&w, &i.target_branch, i.task_id, Some(i.id)).await?;
        if prepared.source_commit != source_commit
            || prepared.target_commit != target_commit
            || !prepared.blockers.is_empty()
        {
            let reason = if prepared.source_commit != source_commit {
                "the source branch moved since you reviewed it"
            } else if prepared.target_commit != target_commit {
                "the target branch moved since you reviewed it"
            } else {
                "something changed since you reviewed it"
            };
            let message = format!("{reason}; inspect the integration again and approve the new commits");
            // The refusal and the end of this operation are one transaction.
            let ending = Ending { op, error: Some(message.clone()) };
            let i = self
                .store_prepared(i, prepared, Some(format!("Approval refused: {reason}.")), Some(ending))
                .await?;
            return Err(stale(&message, &i));
        }

        // 2. The merge commit, created but not yet referenced by any branch.
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let git = self.inner.workspaces.git().clone();
        let tree = match git.merge_tree(&root, &target_commit, &source_commit).await.map_err(git_err)? {
            MergeOutcome::Clean { tree } => tree,
            MergeOutcome::Conflicts { files, .. } => {
                return Err(invalid_state(format!("the merge conflicts in {}", files.join(", "))));
            }
        };
        // The source contains the target, so the result is exactly the source
        // tree that was validated and reviewed.
        let source_tree = git.tree_of(&root, &source_commit).await.map_err(git_err)?;
        if tree != source_tree {
            return Err(invalid_state("the merge result differs from the validated source tree; refusing"));
        }
        let task = match i.task_id {
            Some(t) => self
                .inner
                .storage
                .call(move |c| task_store::find_task(c, t))
                .await
                .map_err(errors::storage)?
                .map(|t| (t.id, t.title)),
            None => None,
        };
        let message = merge_message(
            i.id,
            &w.name,
            &i.source_branch,
            &i.target_branch,
            &source_commit,
            &target_commit,
            task.as_ref().map(|(id, t)| (*id, t.as_str())),
            &actor,
        );
        let merge = git
            .commit_tree(&root, &tree, &[&target_commit, &source_commit], &message)
            .await
            .map_err(git_err)?;
        let checkout = prepared.facts.target_checkout.as_ref().map(|c| PathBuf::from(&c.path));
        let method = if checkout.is_some() { METHOD_CHECKOUT } else { "ref_update" };

        // 3. Durable intent before anything moves.
        {
            let (merge, actor, method) = (merge.clone(), actor.clone(), method.to_owned());
            let (s, t) = (source_commit.clone(), target_commit.clone());
            commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
                let now = now_ms();
                let mut drafts = Vec::new();
                let current = store::get_integration(tx, id)?;
                if current.status != IntegrationStatus::Ready
                    || current.source_commit != s
                    || current.target_commit != t
                {
                    return Ok(Err(ProtocolError::new(
                        ErrorCode::Conflict,
                        "the integration changed meanwhile",
                    )));
                }
                let u = IntegrationUpdate {
                    status: Some(IntegrationStatus::Integrating),
                    status_reason: Some(Some(format!("approved by {actor}"))),
                    approved_by: Some(Some(actor.clone())),
                    approved_at_ms: Some(Some(now)),
                    merge_commit: Some(Some(merge.clone())),
                    method: Some(Some(method.clone())),
                    ..Default::default()
                };
                let after = transition(tx, &current, u, now, &mut drafts)?;
                drafts.insert(
                    0,
                    draft(
                        DomainEvent::IntegrationApproved {
                            integration_id: id,
                            source_commit: s,
                            target_commit: t,
                            merge_commit: merge,
                            method,
                            approved_by: actor,
                        },
                        &context(&after),
                    )?,
                );
                Ok(Ok(((), drafts)))
            })
            .await?;
        }
        let i = self.record(id).await?;

        // 4. Move the target branch, then 5. record what Git shows.
        let outcome = match &checkout {
            Some(path) => {
                let moved = git.fast_forward_checkout(path, &i.target_branch, &target_commit, &merge).await;
                let tip = match git.branch_tip(&root, &i.target_branch).await {
                    Ok(t) => t,
                    Err(e) => return Err(unreadable_result(e)),
                };
                if tip.as_deref() == Some(merge.as_str()) {
                    Ok(None)
                } else {
                    let moved_elsewhere = tip.as_deref() != Some(target_commit.as_str());
                    let reason = match moved {
                        Ok(()) => "the target branch is not at the merge commit after the update".to_owned(),
                        Err(FastForwardError::Refused(m)) => {
                            format!("{m}; Git was not run, so nothing was changed")
                        }
                        Err(e @ FastForwardError::Diverted { .. }) => format!(
                            "{e}; {} was not moved. Inspect the checkout at {} with `git status`",
                            i.target_branch,
                            path.display()
                        ),
                        Err(FastForwardError::Git(e)) => {
                            let what = match &e {
                                GitError::Timeout { .. } => {
                                    format!(
                                        "Git was stopped at its timeout while updating the checkout ({e})"
                                    )
                                }
                                _ => format!("Git did not complete the update of the checkout ({e})"),
                            };
                            describe_checkout_failure(&git, &root, path, &i, &merge, &what, tip.as_deref())
                                .await
                        }
                    };
                    Err((reason, moved_elsewhere))
                }
            }
            None => {
                // The branch may have been checked out since preparation:
                // moving it under a checkout would leave that checkout's files
                // behind its branch.
                match checkout_of(&git, &root, &i.target_branch).await {
                    Err(e) => return Err(unreadable_result(e)),
                    Ok(Some(p)) => Err((
                        format!(
                            "{} was checked out at {} after the integration was prepared; nothing was changed. \
                             Refresh the integration and approve it again",
                            i.target_branch,
                            p.display()
                        ),
                        false,
                    )),
                    Ok(None) => {
                        let reason = format!("switchyard: integrate {} ({})", i.source_branch, i.id);
                        let moved =
                            git.update_branch(&root, &i.target_branch, &merge, &target_commit, &reason).await;
                        let tip = match git.branch_tip(&root, &i.target_branch).await {
                            Ok(t) => t,
                            Err(e) => return Err(unreadable_result(e)),
                        };
                        if tip.as_deref() == Some(merge.as_str()) {
                            // Checked out meanwhile (between the check and the update)?
                            match checkout_of(&git, &root, &i.target_branch).await {
                                Ok(Some(p)) => Ok(Some(format!(
                                    "{} was checked out at {} while it moved: the files there still show {}. \
                                     Inspect that checkout with `git status` before committing there",
                                    i.target_branch,
                                    p.display(),
                                    target_commit
                                ))),
                                _ => Ok(None),
                            }
                        } else {
                            let moved_elsewhere = tip.as_deref() != Some(target_commit.as_str());
                            Err((
                                match moved {
                                    Err(e) => e.to_string(),
                                    Ok(()) => {
                                        "the target branch is not at the merge commit after the update".into()
                                    }
                                },
                                moved_elsewhere,
                            ))
                        }
                    }
                }
            }
        };
        // The outcome and the end of this operation are one transaction.
        self.finish(id, outcome, false, Some(Ending::completed(op))).await?;
        self.wire(id).await
    }

    /// Records the end of an integration that was `integrating`.
    /// With `ending`, the operation that did the work ends in the same
    /// transaction, so `integrated`/`failed` is never visible with that
    /// operation still `running`.
    async fn finish(
        &self,
        id: Uuid,
        outcome: Outcome,
        recovered: bool,
        ending: Option<Ending>,
    ) -> Result<(), ProtocolError> {
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let i = store::get_integration(tx, id)?;
            if i.status != IntegrationStatus::Integrating {
                return Ok(Ok(((), drafts)));
            }
            match outcome {
                Ok(note) => {
                    let merge = i.merge_commit.clone().unwrap_or_default();
                    let mut reason = format!("{} is at {}", i.target_branch, merge);
                    if let Some(n) = note {
                        reason = format!("{reason}. {n}");
                    }
                    let u = IntegrationUpdate {
                        status: Some(IntegrationStatus::Integrated),
                        status_reason: Some(Some(reason)),
                        completed: true,
                        ..Default::default()
                    };
                    let after = transition(tx, &i, u, now, &mut drafts)?;
                    drafts.push(draft(
                        DomainEvent::IntegrationIntegrated {
                            integration_id: id,
                            target_branch: after.target_branch.clone(),
                            merge_commit: merge,
                            source_commit: after.source_commit.clone(),
                            target_commit: after.target_commit.clone(),
                            recovered,
                        },
                        &context(&after),
                    )?);
                }
                Err((reason, target_moved)) => {
                    let u = IntegrationUpdate {
                        status: Some(IntegrationStatus::Failed),
                        status_reason: Some(Some(reason.clone())),
                        failure: Some(Some(reason.clone())),
                        ..Default::default()
                    };
                    let after = transition(tx, &i, u, now, &mut drafts)?;
                    drafts.push(draft(
                        DomainEvent::IntegrationFailed { integration_id: id, reason, target_moved },
                        &context(&after),
                    )?);
                }
            }
            if let Some(ending) = &ending {
                let i = store::get_integration(tx, id)?;
                drafts.extend(ending.record(tx, &i)?);
            }
            Ok(Ok(((), drafts)))
        })
        .await
    }

    /// After a restart: settles what a previous Core left in progress.
    ///
    /// * operations still `running` were interrupted ([`Self::recover_operations`]);
    /// * `integrating`: the target branch decides ([`Self::recover_integrating`]);
    /// * `validating`: the run is `interrupted` (its processes are no longer
    ///   supervised) and the integration is prepared again, or `failed` if
    ///   that is impossible.
    ///
    /// An integration that cannot be settled now (Git cannot be read, or a
    /// Git process may still be writing) stays as it is; a refresh retries.
    pub async fn recover_on_startup(&self) -> Result<(), ProtocolError> {
        self.recover_operations().await?;
        let busy = self.inner.storage.call(|c| store::busy_integrations(c)).await.map_err(errors::storage)?;
        for i in busy {
            if let Err(err) = self.settle_stranded(i.id).await {
                tracing::error!(integration = %i.id, ?err, "cannot settle an interrupted integration");
            }
        }
        Ok(())
    }

    /// Settles an `integrating` integration from the target branch: it
    /// contains the recorded merge commit → `integrated`; otherwise `failed`,
    /// saying what Git shows. Leaves it `integrating` (with the reason) while
    /// the target checkout's `index.lock` suggests a Git process may still be
    /// writing, and returns any Git error instead of guessing.
    pub(crate) async fn recover_integrating(&self, i: &IntegrationRecord) -> Result<(), ProtocolError> {
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let git = self.inner.workspaces.git().clone();
        let tip = git.branch_tip(&root, &i.target_branch).await.map_err(git_err)?;
        // The merge commit is in the target (possibly with commits on top).
        // A merge commit that does not exist (never landed, then pruned) is
        // not in any branch; any other Git failure leaves this undecided.
        let landed = match (tip.as_deref(), i.merge_commit.as_deref()) {
            (Some(t), Some(m)) if t == m => true,
            (Some(t), Some(m)) => {
                git.commit_exists(&root, m).await.map_err(git_err)?
                    && git.is_ancestor(&root, m, t).await.map_err(git_err)?
            }
            _ => false,
        };
        let checkout = if i.method.as_deref() == Some(METHOD_CHECKOUT) {
            checkout_of(&git, &root, &i.target_branch).await.map_err(git_err)?
        } else {
            None
        };
        let outcome: Outcome = if landed {
            Ok(None)
        } else if i.method.as_deref() == Some(METHOD_CHECKOUT) && checkout.is_none() {
            Err((
                format!(
                    "Core stopped before {} was updated; the branch was not moved where it was expected, and it is no \
                     longer checked out. The checkout that was being fast-forwarded may be partially updated: \
                     inspect it with `git status`",
                    i.target_branch
                ),
                tip.as_deref() != Some(i.target_commit.as_str()),
            ))
        } else if let Some(merge) = i.merge_commit.as_deref()
            && let Some(path) = checkout
        {
            if git.repo_state(&path).await.map_err(git_err)?.index_locked {
                let reason = format!(
                    "Waiting: the checkout of {} at {} has an index.lock (a Git process may still be writing, or one \
                     stopped). Refresh once it is gone; remove it only if no Git process is running there.",
                    i.target_branch,
                    path.display()
                );
                self.note_reason(i.id, reason).await?;
                return Ok(());
            }
            let what = "Core stopped while integrating".to_owned();
            Err((
                describe_checkout_failure(&git, &root, &path, i, merge, &what, tip.as_deref()).await,
                tip.as_deref() != Some(i.target_commit.as_str()),
            ))
        } else {
            match tip.as_deref() {
                Some(t) if t == i.target_commit => Err((
                    "Core stopped before the target branch was updated; it was not moved".to_owned(),
                    false,
                )),
                tip => Err((
                    format!(
                        "Core stopped during integration and {} is now at {}, which is neither the approved target \
                         nor the merge commit; inspect it",
                        i.target_branch,
                        tip.unwrap_or("(missing)")
                    ),
                    true,
                )),
            }
        };
        tracing::warn!(integration = %i.id, ok = outcome.is_ok(), "settled an interrupted integration");
        self.finish(i.id, outcome, true, None).await
    }

    /// Updates the status reason without changing the status.
    async fn note_reason(&self, id: Uuid, reason: String) -> Result<(), ProtocolError> {
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let i = store::get_integration(tx, id)?;
            let u = IntegrationUpdate { status_reason: Some(Some(reason)), ..Default::default() };
            store::update_integration(tx, id, i.status, &u, now_ms())?;
            Ok(Ok(((), Vec::new())))
        })
        .await
    }
}

/// The branch could not be read after moving it: the integration stays
/// `integrating`, and a refresh (or the next start) settles it from Git.
fn unreadable_result(e: GitError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::GitFailed,
        format!(
            "the target branch could not be read after the update ({e}); the integration stays integrating until a \
             refresh settles it from Git"
        ),
    )
}

fn stale(message: &str, i: &IntegrationRecord) -> ProtocolError {
    ProtocolError::new(ErrorCode::Conflict, message.to_owned()).with_data(serde_json::json!({
        "source_commit": i.source_commit,
        "target_commit": i.target_commit,
        "status": i.status,
        "blockers": blockers_of(i),
    }))
}
