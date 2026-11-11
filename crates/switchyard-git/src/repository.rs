use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::command::Git;
use crate::error::GitError;

/// A validated Git working-tree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRoot {
    /// Canonical absolute path (symlinks resolved).
    pub path: PathBuf,
    /// Default display name: the directory's final component.
    pub name: String,
}

fn io_error(path: &Path, source: std::io::Error) -> GitError {
    match source.kind() {
        ErrorKind::NotFound => GitError::NotFound(path.to_path_buf()),
        ErrorKind::PermissionDenied => GitError::PermissionDenied(path.to_path_buf()),
        _ => GitError::Io { path: path.to_path_buf(), source },
    }
}

/// Checks that `path` is an existing, readable directory and returns its
/// canonical form.
pub async fn accessible_directory(path: &Path) -> Result<PathBuf, GitError> {
    if !path.is_absolute() {
        return Err(GitError::NotAbsolute(path.to_path_buf()));
    }
    let meta = tokio::fs::metadata(path).await.map_err(|e| io_error(path, e))?;
    if !meta.is_dir() {
        return Err(GitError::NotADirectory(path.to_path_buf()));
    }
    let canonical = tokio::fs::canonicalize(path).await.map_err(|e| io_error(path, e))?;
    // Listing proves read + execute permission on the directory itself.
    drop(tokio::fs::read_dir(&canonical).await.map_err(|e| io_error(&canonical, e))?);
    Ok(canonical)
}

impl Git {
    /// Validates that `path` is the root of a non-bare Git working tree.
    pub async fn resolve_repository_root(&self, path: &Path) -> Result<RepositoryRoot, GitError> {
        let canonical = accessible_directory(path).await?;
        let args = ["rev-parse", "--is-inside-work-tree", "--show-toplevel"];
        let out = self.run(&canonical, &args).await?;
        if !out.success {
            if out.stderr.contains("not a git repository") {
                return Err(GitError::NotARepository { path: canonical, reason: out.stderr });
            }
            if out.stderr.contains("must be run in a work tree") {
                return Err(GitError::NotARepository {
                    path: canonical,
                    reason: "bare repository or Git metadata directory".into(),
                });
            }
            return Err(Git::failed(&args, out));
        }
        let mut lines = out.stdout.lines();
        if lines.next() != Some("true") {
            return Err(GitError::NotARepository {
                path: canonical,
                reason: "not inside a working tree".into(),
            });
        }
        let toplevel = lines
            .next()
            .filter(|l| !l.is_empty())
            .ok_or_else(|| GitError::UnexpectedOutput(out.stdout.clone()))?;
        let root = tokio::fs::canonicalize(toplevel).await.map_err(|e| io_error(Path::new(toplevel), e))?;
        if root != canonical {
            return Err(GitError::NotRepositoryRoot { path: canonical, root });
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| root.display().to_string());
        Ok(RepositoryRoot { path: root, name })
    }
}
