//! Approval and integration (ADRs 0028, 0031).
//!
//! 1. Under the project's repository lock, Core re-reads every fact and
//!    checks that the source and target are still exactly the commits the
//!    operator approved, and that nothing blocks.
//! 2. It computes the merge in memory and creates the merge commit object
//!    (parents: target, source). Nothing has moved yet.
//! 3. It records `integrating` with that merge commit, durably.
//! 4. It moves the target branch: one compare-and-swap ref update, or, if
//!    the target is checked out, a fast-forward of that clean checkout,
//!    which Git refuses rather than overwrite anything.
//! 5. It records `integrated`, or `failed` with what the branch points to.
//!
//! After a crash between 3 and 5, recovery compares the target branch with
//! the recorded merge commit (see [`IntegrationService::recover_on_startup`]).

use std::path::Path;

use switchyard_events::{DomainEvent, now_ms};
use switchyard_git::MergeOutcome;
use switchyard_integrations::{IntegrationStatus, RunStatus, StepStatus, merge_message};
use switchyard_protocol::model::Integration;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{self as store, IntegrationRecord, IntegrationUpdate, StepOutcome};
use switchyard_storage::tasks as task_store;
use uuid::Uuid;

use super::{IntegrationService, blockers_of, context, git_err, invalid_state, transition};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

impl IntegrationService {
    /// The operator approves delivering exactly `source_commit` into the
    /// target branch at exactly `target_commit`. Only this method moves a
    /// target branch, and only Core's operator IPC reaches it.
    pub async fn approve(
        &self,
        id: Uuid,
        source_commit: String,
        target_commit: String,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
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
            let i = self.store_prepared(i, prepared, Some(format!("Approval refused: {reason}."))).await?;
            return Err(stale(
                &format!("{reason}; inspect the integration again and approve the new commits"),
                &i,
            ));
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
        let checkout = prepared.facts.target_checkout.as_ref().map(|c| c.path.clone());
        let method = if checkout.is_some() { "checkout_fast_forward" } else { "ref_update" };

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

        // 4. Move the target branch.
        let reason = format!("switchyard: integrate {} ({})", i.source_branch, i.id);
        let moved = match &checkout {
            Some(path) => {
                // `merge --ff-only` acts on whatever the checkout has checked out:
                // re-check, immediately before, that it is still the target branch
                // at exactly the approved commit.
                let wanted = format!("refs/heads/{}", i.target_branch);
                match git.checkout_head(Path::new(path)).await {
                    Ok((Some(b), Some(h))) if b == wanted && h == target_commit => {
                        git.fast_forward_checkout(Path::new(path), &merge).await
                    }
                    Ok(_) => Err(switchyard_git::GitError::WorktreeBusy(format!(
                        "the checkout at {path} is no longer on {} at {}; nothing was changed",
                        i.target_branch, target_commit
                    ))),
                    Err(e) => Err(e),
                }
            }
            None => git.update_branch(&root, &i.target_branch, &merge, &target_commit, &reason).await,
        };

        // 5. Record what actually happened, from Git. If the branch cannot be
        // read now, the integration stays `integrating` and recovery settles
        // it at the next start.
        let tip = git.branch_tip(&root, &i.target_branch).await.map_err(git_err)?;
        let outcome = match (&moved, tip.as_deref()) {
            (_, Some(t)) if t == merge => Ok(()),
            (Err(e), _) => Err((e.to_string(), tip.as_deref() != Some(target_commit.as_str()))),
            (Ok(()), _) => {
                Err(("the target branch is not at the merge commit after the update".into(), true))
            }
        };
        self.finish(id, outcome, false).await?;
        self.wire(id).await
    }

    /// Records the end of an integration that was `integrating`.
    async fn finish(
        &self,
        id: Uuid,
        outcome: Result<(), (String, bool)>,
        recovered: bool,
    ) -> Result<(), ProtocolError> {
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let i = store::get_integration(tx, id)?;
            if i.status != IntegrationStatus::Integrating {
                return Ok(Ok(((), drafts)));
            }
            match outcome {
                Ok(()) => {
                    let merge = i.merge_commit.clone().unwrap_or_default();
                    let u = IntegrationUpdate {
                        status: Some(IntegrationStatus::Integrated),
                        status_reason: Some(Some(format!("{} is at {}", i.target_branch, merge))),
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
            Ok(Ok(((), drafts)))
        })
        .await
    }

    /// After a restart: settles what a previous Core left in progress.
    ///
    /// * `integrating`: the target branch decides. At the recorded merge
    ///   commit → `integrated`; still at the approved target commit →
    ///   `failed` (nothing changed); anywhere else → `failed`, target moved.
    /// * `validating`: the run is `interrupted` (its processes are no longer
    ///   supervised) and the integration is prepared again.
    pub async fn recover_on_startup(&self) -> Result<(), ProtocolError> {
        let (busy, runs) = self
            .inner
            .storage
            .call(|c| Ok((store::busy_integrations(c)?, store::running_runs(c)?)))
            .await
            .map_err(errors::storage)?;
        for run in runs {
            let rid = run.id;
            self.inner
                .storage
                .call(move |c| {
                    let now = now_ms();
                    for s in store::steps_for_run(c, rid)? {
                        if !s.status.is_final() {
                            let o = StepOutcome {
                                detail: Some("Core stopped while this ran; the result is unknown".into()),
                                ..Default::default()
                            };
                            store::finish_step(c, s.id, StepStatus::Cancelled, &o, now)?;
                        }
                    }
                    store::finish_run(c, rid, RunStatus::Interrupted, now)
                })
                .await
                .map_err(errors::storage)?;
        }
        for i in busy {
            match i.status {
                IntegrationStatus::Integrating => {
                    if let Err(err) = self.recover_integrating(&i).await {
                        tracing::error!(integration = %i.id, ?err, "cannot settle an interrupted integration");
                    }
                }
                IntegrationStatus::Validating => {
                    if let Err(err) = self.prepare(i.id).await {
                        tracing::warn!(integration = %i.id, ?err, "cannot prepare an interrupted validation again");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn recover_integrating(&self, i: &IntegrationRecord) -> Result<(), ProtocolError> {
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let tip = self.inner.workspaces.git().branch_tip(&root, &i.target_branch).await.map_err(git_err)?;
        let git = self.inner.workspaces.git();
        // The merge commit is in the target (possibly with commits on top).
        let landed = match (tip.as_deref(), i.merge_commit.as_deref()) {
            (Some(t), Some(m)) => t == m || git.is_ancestor(&root, m, t).await.unwrap_or(false),
            _ => false,
        };
        let outcome = match (tip.as_deref(), i.merge_commit.as_deref()) {
            _ if landed => Ok(()),
            (Some(t), _) if t == i.target_commit && i.method.as_deref() == Some("checkout_fast_forward") => {
                Err((
                    format!(
                        "Core stopped before {} was updated; the branch was not moved, but if Core stopped during the \
                     fast-forward the checkout's files may be partly updated: check it with `git status`",
                        i.target_branch
                    ),
                    false,
                ))
            }
            (Some(t), _) if t == i.target_commit => {
                Err(("Core stopped before the target branch was updated; nothing changed".to_owned(), false))
            }
            (tip, _) => Err((
                format!(
                    "Core stopped during integration and {} is now at {}, which is neither the approved target nor \
                     the merge commit; inspect it",
                    i.target_branch,
                    tip.unwrap_or("(missing)")
                ),
                true,
            )),
        };
        tracing::warn!(integration = %i.id, ok = outcome.is_ok(), "settled an interrupted integration");
        self.finish(i.id, outcome, true).await
    }
}

fn stale(message: &str, i: &IntegrationRecord) -> ProtocolError {
    ProtocolError::new(ErrorCode::Conflict, message.to_owned()).with_data(serde_json::json!({
        "source_commit": i.source_commit,
        "target_commit": i.target_commit,
        "status": i.status,
        "blockers": blockers_of(i),
    }))
}
