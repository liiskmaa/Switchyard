//! Facts about a workspace on disk and in Git, and the validation that
//! every session start, verify and reconciliation relies on.
//!
//! Nothing here writes anything. All paths compared are canonical.

use std::path::{Path, PathBuf};

use switchyard_git::GitStatus;
use switchyard_git::worktree::WorktreeIdentity;
use switchyard_protocol::ErrorCode;
use switchyard_storage::WorkspaceRecord;
use switchyard_workspaces::{WorkspaceKind, worktree_path};

use super::WorkspaceService;

/// Why a worktree is not usable. `code` is persisted as the prefix of
/// `status_reason` and is stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    pub error: ErrorCode,
    pub code: &'static str,
    pub detail: String,
}

impl Problem {
    fn new(error: ErrorCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self { error, code, detail: detail.into() }
    }

    pub fn reason(&self) -> String {
        format!("{}: {}", self.code, self.detail)
    }
}

/// What a full inspection found.
pub(crate) struct Inspection {
    pub repository_available: bool,
    /// The project's shared Git directory (when the repository is available).
    pub common_dir: Option<PathBuf>,
    pub directory_exists: bool,
    pub identity: Option<WorktreeIdentity>,
    /// First validation failure, if any.
    pub problem: Option<Problem>,
}

/// `true` if `path` is a real directory (not a symlink to one).
pub(crate) async fn real_dir(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok_and(|m| m.is_dir())
}

impl WorkspaceService {
    /// The path the workspace must have, derived from its ids.
    pub(crate) fn expected_path(&self, w: &WorkspaceRecord) -> PathBuf {
        worktree_path(&self.inner.root, w.project_id, w.id)
    }

    /// Validates a worktree workspace against its record: Core-owned path,
    /// repository available, directory present, a worktree of the project's
    /// repository rooted exactly at the path, with the recorded branch
    /// checked out.
    pub(crate) async fn inspect(&self, w: &WorkspaceRecord, project_root: &Path) -> Inspection {
        let mut out = Inspection {
            repository_available: false,
            common_dir: None,
            directory_exists: false,
            identity: None,
            problem: None,
        };
        let git = &self.inner.git;
        if w.kind != WorkspaceKind::Worktree {
            out.problem = Some(Problem::new(
                ErrorCode::LegacyWorkspace,
                "legacy_checkout",
                "this record is the project checkout from before workspace isolation",
            ));
            return out;
        }
        let path = PathBuf::from(&w.path);
        if path != self.expected_path(w) {
            out.problem = Some(Problem::new(
                ErrorCode::WorktreeIdentityMismatch,
                "path_mismatch",
                format!("recorded path {} is not the Core-owned location for this workspace", w.path),
            ));
            return out;
        }
        match git.resolve_repository_root(project_root).await {
            Ok(root) if root.path == project_root => {
                out.repository_available = true;
                out.common_dir = git.worktree_identity(project_root).await.ok().map(|i| i.common_dir);
            }
            Ok(root) => {
                out.problem = Some(Problem::new(
                    ErrorCode::RepositoryUnavailable,
                    "repository_unavailable",
                    format!(
                        "{} is no longer a repository root (root is {})",
                        project_root.display(),
                        root.path.display()
                    ),
                ));
            }
            Err(err) => {
                out.problem = Some(Problem::new(
                    ErrorCode::RepositoryUnavailable,
                    "repository_unavailable",
                    err.to_string(),
                ));
            }
        }
        match tokio::fs::symlink_metadata(&path).await {
            Ok(m) if m.is_dir() => out.directory_exists = true,
            Ok(_) => {
                out.problem.get_or_insert_with(|| {
                    Problem::new(
                        ErrorCode::WorktreeIdentityMismatch,
                        "not_a_directory",
                        format!("{} exists but is not a plain directory", w.path),
                    )
                });
                return out;
            }
            Err(_) => {
                out.problem.get_or_insert_with(|| {
                    Problem::new(
                        ErrorCode::WorktreeMissing,
                        "missing_worktree",
                        format!("{} does not exist", w.path),
                    )
                });
                return out;
            }
        }
        if !out.repository_available {
            return out;
        }
        let identity = match git.worktree_identity(&path).await {
            Ok(i) => i,
            Err(err) => {
                out.problem = Some(Problem::new(
                    ErrorCode::WorktreeIdentityMismatch,
                    "not_a_worktree",
                    err.to_string(),
                ));
                return out;
            }
        };
        let expected_ref = w.branch.as_deref().map(|b| format!("refs/heads/{b}"));
        if identity.toplevel != path {
            out.problem = Some(Problem::new(
                ErrorCode::WorktreeIdentityMismatch,
                "not_a_worktree",
                format!(
                    "{} is not a worktree root (Git resolves it to {})",
                    w.path,
                    identity.toplevel.display()
                ),
            ));
        } else if out.common_dir.as_ref() != Some(&identity.common_dir) {
            out.problem = Some(Problem::new(
                ErrorCode::WorktreeIdentityMismatch,
                "wrong_repository",
                format!("{} belongs to a different repository than the project", w.path),
            ));
        } else if identity.head_ref != expected_ref {
            out.problem = Some(Problem::new(
                ErrorCode::WorktreeIdentityMismatch,
                "branch_mismatch",
                format!(
                    "expected branch {} but {} is checked out",
                    w.branch.as_deref().unwrap_or("?"),
                    identity
                        .head_ref
                        .as_deref()
                        .map(|r| r.trim_start_matches("refs/heads/"))
                        .unwrap_or("a detached HEAD")
                ),
            ));
        }
        out.identity = Some(identity);
        out
    }

    /// `git status` of a workspace directory, `None` if it cannot be read.
    pub(crate) async fn status_of(&self, w: &WorkspaceRecord) -> Option<GitStatus> {
        match self.inner.git.status(Path::new(&w.path)).await {
            Ok(s) => Some(s),
            Err(err) => {
                tracing::warn!(workspace_id = %w.id, %err, "cannot read workspace git status");
                None
            }
        }
    }
}
