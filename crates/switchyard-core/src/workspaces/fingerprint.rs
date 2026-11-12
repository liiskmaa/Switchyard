//! Complete, deterministic scan of a worktree directory for cleanup.
//!
//! `git status` cannot establish what `git worktree remove` would delete: it
//! reports an ignored directory as a single entry whatever it contains. So
//! cleanup scans the directory itself.
//!
//! The scan:
//! - visits every entry with `lstat`, never following symlinks; a symlink's
//!   target string is hashed, the target is never visited;
//! - hashes, in sorted order:
//!   - the relative path and file type;
//!   - size, mode, owner and inode;
//!   - modification time and **change time** (`ctime`, maintained by the
//!     kernel and not settable by ordinary tools). A content edit therefore
//!     changes the fingerprint even when `mtime` is restored.
//!
//! File contents are not read or hashed: that would cost time proportional to
//! the total size (large build outputs and dependency trees), and `ctime`
//! already changes on every write.
//!
//! These are reported as problems (each blocks cleanup):
//! - **Unreadable entries:** a directory that cannot be listed, any `lstat`
//!   failure, or a file without owner-read permission. Their contents are
//!   unknown.
//! - **Other filesystems:** a directory on another device (a mount point).
//!   Recursive removal would cross into it.
//! - **Foreign ownership or missing permissions:** entries owned by another
//!   user, or directories without owner write/execute permission. Removal
//!   could fail halfway and leave a partly deleted worktree.
//! - **Nested repositories:** any `.git` below the root. They may hold
//!   commits that exist nowhere else.
//! - **Size:** more entries than `max_entries`. The tree is then not fully
//!   verified.
//!
//! The cost is one `lstat` per entry, roughly a second per 100k entries on an
//! SSD. It runs on the blocking pool, twice per cleanup (review and the final
//! check).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use switchyard_workspaces::{TreeProblem, TreeSummary};

/// Entries scanned before giving up (`too_large` blocks cleanup).
pub(crate) const MAX_ENTRIES: u64 = 2_000_000;
/// Problems recorded (the first one of each kind already blocks cleanup).
const MAX_PROBLEMS: usize = 50;

fn problem(problems: &mut Vec<TreeProblem>, code: &str, rel: &Path, detail: impl Into<String>) {
    if problems.len() < MAX_PROBLEMS {
        problems.push(TreeProblem {
            code: code.into(),
            path: if rel.as_os_str().is_empty() { ".".into() } else { rel.display().to_string() },
            detail: detail.into(),
        });
    }
}

/// Scans `root` (which must be a real directory, not a symlink).
pub(crate) fn scan(root: &Path, max_entries: u64) -> TreeSummary {
    let mut summary = TreeSummary::default();
    let mut hasher = DefaultHasher::new();
    let root_meta = match std::fs::symlink_metadata(root) {
        Ok(m) if m.is_dir() => m,
        Ok(_) => {
            problem(
                &mut summary.problems,
                "unreadable",
                Path::new(""),
                "the worktree path is not a directory",
            );
            return summary;
        }
        Err(e) => {
            problem(&mut summary.problems, "unreadable", Path::new(""), e.to_string());
            return summary;
        }
    };
    let (dev, uid) = (root_meta.dev(), root_meta.uid());
    check_dir_permissions(&root_meta, Path::new(""), &mut summary.problems);

    // Depth-first over directories; entries within a directory are sorted
    // so the fingerprint does not depend on readdir order.
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        let abs_dir = root.join(&rel_dir);
        let reader = match std::fs::read_dir(&abs_dir) {
            Ok(r) => r,
            Err(e) => {
                problem(&mut summary.problems, "unreadable", &rel_dir, format!("cannot list directory: {e}"));
                continue;
            }
        };
        let mut names = Vec::new();
        for entry in reader {
            match entry {
                Ok(e) => names.push(e.file_name()),
                Err(e) => problem(
                    &mut summary.problems,
                    "unreadable",
                    &rel_dir,
                    format!("cannot list directory: {e}"),
                ),
            }
        }
        names.sort();
        let mut subdirs = Vec::new();
        for name in names {
            summary.entries += 1;
            if summary.entries > max_entries {
                problem(
                    &mut summary.problems,
                    "too_large",
                    Path::new(""),
                    format!("more than {max_entries} entries"),
                );
                summary.fingerprint = hasher.finish();
                return summary;
            }
            let rel = rel_dir.join(&name);
            let meta = match std::fs::symlink_metadata(root.join(&rel)) {
                Ok(m) => m,
                Err(e) => {
                    problem(&mut summary.problems, "unreadable", &rel, e.to_string());
                    continue;
                }
            };
            let ft = meta.file_type();
            let kind: u8 = if ft.is_symlink() {
                b'l'
            } else if ft.is_dir() {
                b'd'
            } else if ft.is_file() {
                b'f'
            } else if ft.is_fifo() {
                b'p'
            } else if ft.is_socket() {
                b's'
            } else {
                b'o'
            };
            rel.as_os_str().as_bytes().hash(&mut hasher);
            kind.hash(&mut hasher);
            (meta.len(), meta.mode(), meta.uid(), meta.ino()).hash(&mut hasher);
            (meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec()).hash(&mut hasher);
            if kind == b'l' {
                match std::fs::read_link(root.join(&rel)) {
                    Ok(target) => target.as_os_str().as_bytes().hash(&mut hasher),
                    Err(e) => {
                        problem(&mut summary.problems, "unreadable", &rel, format!("cannot read link: {e}"))
                    }
                }
            }
            if kind == b'f' {
                summary.bytes = summary.bytes.saturating_add(meta.len());
                if meta.mode() & 0o400 == 0 {
                    problem(&mut summary.problems, "unreadable", &rel, "file is not readable by its owner");
                }
            }
            if meta.uid() != uid {
                problem(&mut summary.problems, "foreign_owner", &rel, format!("owned by uid {}", meta.uid()));
            }
            // A `.git` anywhere below the root (file or directory) is a
            // nested repository or worktree. The root's own `.git` file is
            // this worktree's link and is expected.
            if name == ".git" && !rel_dir.as_os_str().is_empty() {
                problem(&mut summary.problems, "nested_repository", &rel_dir, "contains its own .git");
            }
            if kind == b'd' {
                if meta.dev() != dev {
                    problem(
                        &mut summary.problems,
                        "foreign_filesystem",
                        &rel,
                        "a different filesystem is mounted here",
                    );
                    continue;
                }
                check_dir_permissions(&meta, &rel, &mut summary.problems);
                subdirs.push(rel);
            }
        }
        // Reverse so the traversal order is ascending (stack is LIFO).
        stack.extend(subdirs.into_iter().rev());
    }
    summary.fingerprint = hasher.finish();
    summary
}

fn check_dir_permissions(meta: &std::fs::Metadata, rel: &Path, problems: &mut Vec<TreeProblem>) {
    if meta.mode() & 0o700 != 0o700 {
        let code = if meta.mode() & 0o500 != 0o500 { "unreadable" } else { "not_removable" };
        problem(problems, code, rel, format!("directory mode {:o}", meta.mode() & 0o777));
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".git"), "gitdir: x").unwrap();
        std::fs::create_dir_all(root.join("build/sub")).unwrap();
        std::fs::write(root.join("build/out.bin"), "x").unwrap();
        std::fs::write(root.join("README.md"), "hi").unwrap();
        dir
    }

    #[test]
    fn deterministic_and_sensitive_to_nested_changes() {
        let dir = tree();
        let a = scan(dir.path(), MAX_ENTRIES);
        assert_eq!(scan(dir.path(), MAX_ENTRIES), a, "stable when nothing changes");
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        assert_eq!((a.entries, a.bytes), (5, 1 + 2 + 9));

        std::fs::write(dir.path().join("build/sub/new.env"), "SECRET=1").unwrap();
        let b = scan(dir.path(), MAX_ENTRIES);
        assert_ne!(a.fingerprint, b.fingerprint, "a new file deep in an ignored directory is detected");

        std::fs::write(dir.path().join("build/out.bin"), "y").unwrap(); // same size
        let c = scan(dir.path(), MAX_ENTRIES);
        assert_ne!(b.fingerprint, c.fingerprint, "a same-size content change is detected (ctime/mtime)");
    }

    #[test]
    fn symlinks_are_hashed_not_followed() {
        let dir = tree();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("big")).unwrap();
        std::fs::write(outside.path().join("big/file"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path().join("big"), dir.path().join("build/link")).unwrap();
        let a = scan(dir.path(), MAX_ENTRIES);
        assert_eq!(a.entries, 6, "the link is one entry; its target is not visited");
        assert!(a.problems.is_empty());
        std::fs::remove_file(dir.path().join("build/link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("build/link")).unwrap();
        assert_ne!(scan(dir.path(), MAX_ENTRIES).fingerprint, a.fingerprint, "retargeting is detected");
    }

    #[test]
    fn unsafe_content_is_reported() {
        let dir = tree();
        std::fs::create_dir_all(dir.path().join("build/vendor/.git")).unwrap();
        let locked = dir.path().join("build/sub");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable_file = dir.path().join("README.md");
        std::fs::set_permissions(&unreadable_file, std::fs::Permissions::from_mode(0o200)).unwrap();
        let s = scan(dir.path(), MAX_ENTRIES);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let codes: Vec<(&str, &str)> =
            s.problems.iter().map(|p| (p.code.as_str(), p.path.as_str())).collect();
        assert!(codes.contains(&("nested_repository", "build/vendor")), "{codes:?}");
        assert!(codes.contains(&("unreadable", "build/sub")), "{codes:?}");
        assert!(codes.contains(&("unreadable", "README.md")), "{codes:?}");
        assert!(scan(dir.path(), 2).problems.iter().any(|p| p.code == "too_large"));
    }
}
