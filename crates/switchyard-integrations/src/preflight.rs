//! What prevents an integration, decided from facts Core read from Git and
//! its own records (ADR 0029). Every fact comes from Git or Core's database,
//! never from an agent's report.

use serde::{Deserialize, Serialize};

/// The merge of the target into the source, computed without touching
/// any working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeFact {
    /// The target commit is already contained in the source commit: the
    /// merge result is exactly the source tree.
    UpToDate,
    /// The target moved on, but merging it into the source is clean.
    Clean,
    /// Merging the target into the source conflicts in these files.
    Conflicts { files: Vec<String> },
}

/// The required review, checked against the exact content being integrated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFact {
    pub required: bool,
    /// The latest review approved, and the tree it reviewed is the source tree.
    pub satisfied: bool,
    pub detail: String,
}

/// The target branch is checked out in a working tree (usually the
/// project checkout): integrating moves that checkout too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetCheckoutFact {
    pub path: String,
    /// Staged, unstaged or conflicted tracked changes.
    pub has_local_changes: bool,
    /// Operations in progress (merge, rebase, index.lock).
    pub busy: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// The workspace record, its worktree and its branch were verified.
    pub workspace_valid: Result<(), String>,
    /// An agent session is attached to the workspace.
    pub live_session: bool,
    /// The worktree has uncommitted or untracked (non-ignored) changes.
    pub uncommitted_changes: bool,
    /// Operations in progress in the workspace.
    pub workspace_busy: Vec<String>,
    /// Commits on the source that the target lacks.
    pub source_ahead: u32,
    /// The source tree differs from the target tree.
    pub has_changes: bool,
    pub merge: MergeFact,
    /// `None` if the target branch is not checked out anywhere.
    pub target_checkout: Option<TargetCheckoutFact>,
    /// Git can create commits (a committer identity is configured).
    pub identity: Result<String, String>,
    /// The task allows integration (`None`: no task linked).
    pub task: Option<Result<(), String>>,
    pub review: ReviewFact,
    /// Enabled validation commands configured for the project.
    pub validations_configured: u32,
    /// A validation run for exactly the source commit passed.
    pub validation_passed: bool,
    /// The latest run for the source commit failed (or was cancelled).
    pub validation_failed: bool,
}

/// One reason an integration cannot be approved now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub code: String,
    pub message: String,
}

fn b(code: &str, message: impl Into<String>) -> Blocker {
    Blocker { code: code.into(), message: message.into() }
}

/// Everything that prevents approval, in the order the operator should
/// address it. Empty means the integration is ready for approval.
pub fn blockers(f: &Facts) -> Vec<Blocker> {
    let mut v = Vec::new();
    if let Err(e) = &f.workspace_valid {
        v.push(b("workspace_invalid", format!("The workspace cannot be used: {e}")));
    }
    if f.live_session {
        v.push(b("agent_working", "An agent session is running in the workspace; stop it first."));
    }
    if !f.workspace_busy.is_empty() && !matches!(f.merge, MergeFact::Conflicts { .. }) {
        v.push(b("workspace_busy", format!("In the workspace, {}.", f.workspace_busy.join("; "))));
    }
    if f.uncommitted_changes {
        v.push(b(
            "uncommitted_changes",
            "The workspace has uncommitted changes. Commit them in the workspace first, so that exactly what you \
             inspect is integrated.",
        ));
    }
    if !f.has_changes && f.source_ahead == 0 {
        v.push(b("no_changes", "The source contains nothing that the target branch does not already have."));
    }
    match &f.merge {
        MergeFact::UpToDate => {}
        MergeFact::Clean => v.push(b(
            "out_of_date",
            "The target branch has new commits. Update the workspace from the target (a merge inside the \
             workspace), then validate the result.",
        )),
        MergeFact::Conflicts { files } => v.push(b(
            "conflicts",
            format!(
                "The target branch has new commits that conflict with the workspace in {} file(s): {}. Resolve them \
                 in the workspace; the target branch is never changed to resolve conflicts.",
                files.len(),
                files.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
            ),
        )),
    }
    if let Some(c) = &f.target_checkout {
        if c.has_local_changes {
            v.push(b(
                "target_checkout_dirty",
                format!(
                    "The target branch is checked out at {} with uncommitted changes. Commit or stash them there \
                     first; Switchyard never overwrites them.",
                    c.path
                ),
            ));
        }
        if !c.busy.is_empty() {
            v.push(b(
                "target_checkout_busy",
                format!("In the checkout at {}, {}.", c.path, c.busy.join("; ")),
            ));
        }
    }
    if let Err(e) = &f.identity {
        v.push(b("identity_missing", format!("Git cannot create the merge commit: {e}")));
    }
    if let Some(Err(e)) = &f.task {
        v.push(b("task_not_eligible", e.clone()));
    }
    if f.review.required && !f.review.satisfied {
        v.push(b("review_required", f.review.detail.clone()));
    }
    if f.validations_configured > 0 && !f.validation_passed {
        if f.validation_failed {
            v.push(b("validation_failed", "Validation failed for this source commit."));
        } else {
            v.push(b("validation_required", "Run the project's validation commands for this source commit."));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Facts {
        Facts {
            workspace_valid: Ok(()),
            live_session: false,
            uncommitted_changes: false,
            workspace_busy: vec![],
            source_ahead: 2,
            has_changes: true,
            merge: MergeFact::UpToDate,
            target_checkout: Some(TargetCheckoutFact {
                path: "/repo".into(),
                has_local_changes: false,
                busy: vec![],
            }),
            identity: Ok("A <a@b>".into()),
            task: Some(Ok(())),
            review: ReviewFact { required: true, satisfied: true, detail: String::new() },
            validations_configured: 1,
            validation_passed: true,
            validation_failed: false,
        }
    }

    fn codes(f: &Facts) -> Vec<String> {
        blockers(f).into_iter().map(|b| b.code).collect()
    }

    #[test]
    fn a_clean_bound_integration_has_no_blockers() {
        assert!(blockers(&ready()).is_empty());
        let none = Facts { validations_configured: 0, validation_passed: false, ..ready() };
        assert!(blockers(&none).is_empty(), "no validation configured is allowed (and shown)");
    }

    #[test]
    fn each_unsafe_fact_blocks() {
        let cases: Vec<(Facts, &str)> = vec![
            (Facts { workspace_valid: Err("missing".into()), ..ready() }, "workspace_invalid"),
            (Facts { live_session: true, ..ready() }, "agent_working"),
            (Facts { uncommitted_changes: true, ..ready() }, "uncommitted_changes"),
            (Facts { workspace_busy: vec!["a rebase is in progress".into()], ..ready() }, "workspace_busy"),
            (Facts { has_changes: false, source_ahead: 0, ..ready() }, "no_changes"),
            (Facts { merge: MergeFact::Clean, ..ready() }, "out_of_date"),
            (Facts { merge: MergeFact::Conflicts { files: vec!["a".into()] }, ..ready() }, "conflicts"),
            (
                Facts {
                    target_checkout: Some(TargetCheckoutFact {
                        path: "/r".into(),
                        has_local_changes: true,
                        busy: vec![],
                    }),
                    ..ready()
                },
                "target_checkout_dirty",
            ),
            (Facts { identity: Err("no name".into()), ..ready() }, "identity_missing"),
            (Facts { task: Some(Err("task is failed".into())), ..ready() }, "task_not_eligible"),
            (
                Facts {
                    review: ReviewFact { required: true, satisfied: false, detail: "x".into() },
                    ..ready()
                },
                "review_required",
            ),
            (Facts { validation_passed: false, ..ready() }, "validation_required"),
            (Facts { validation_passed: false, validation_failed: true, ..ready() }, "validation_failed"),
        ];
        for (facts, code) in cases {
            assert_eq!(codes(&facts), vec![code.to_owned()], "{code}");
        }
    }

    #[test]
    fn a_review_that_is_not_required_does_not_block() {
        let f = Facts {
            review: ReviewFact { required: false, satisfied: false, detail: String::new() },
            ..ready()
        };
        assert!(blockers(&f).is_empty());
    }
}
