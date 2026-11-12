//! Path facts: where each target of an operation lies relative to the
//! session's workspace (ADR 0017). This is the only file system I/O on the
//! policy path; it runs before evaluation, outside any lock.

use std::path::{Component, Path, PathBuf};

use switchyard_policy::{Operation, OperationCategory, PathFact};

fn unresolved(reason: impl Into<String>) -> PathFact {
    PathFact::Unresolved { reason: reason.into() }
}

/// Lexical normalization: removes `.` and resolves `..` without touching the
/// file system. `None` if `..` climbs above the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) => return None,
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() || out.as_os_str().is_empty() {
                    return None;
                }
            }
            Component::Normal(n) => out.push(n),
        }
    }
    Some(out)
}

/// `path` relative to `root`, `/`-separated; `None` if outside or not UTF-8.
fn relative(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// Canonicalizes the deepest existing ancestor of `path` (following
/// symlinks) and re-appends the components that do not exist yet.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => {
                // It exists (possibly as a symlink): it must resolve. A
                // dangling link would redirect a write to an unknown place.
                let mut real = std::fs::canonicalize(current)
                    .map_err(|e| format!("{} cannot be resolved: {e}", current.display()))?;
                for part in missing.iter().rev() {
                    real.push(part);
                }
                return Ok(real);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                    return Err("no existing ancestor".into());
                };
                missing.push(name);
                current = parent;
            }
            Err(e) => return Err(format!("{} cannot be inspected: {e}", current.display())),
        }
    }
}

fn fact(workspace: &Path, canonical_root: &Path, target: &str) -> PathFact {
    if target.starts_with('~') {
        return unresolved("home-relative paths are not resolved");
    }
    let requested = Path::new(target);
    let absolute = if requested.is_absolute() { requested.to_path_buf() } else { workspace.join(requested) };
    let Some(lexical) = normalize(&absolute) else { return PathFact::Outside };
    let Some(lexical_rel) = relative(&lexical, workspace).or_else(|| relative(&lexical, canonical_root))
    else {
        return PathFact::Outside;
    };
    let resolved = match resolve(&lexical) {
        Ok(p) => p,
        Err(reason) => return unresolved(reason),
    };
    match relative(&resolved, canonical_root) {
        Some(resolved_rel) => PathFact::Inside { lexical: lexical_rel, resolved: resolved_rel },
        None => PathFact::Outside,
    }
}

/// One fact per target of `op`. A search without an explicit path targets
/// the workspace root. Non-path operations have no facts.
pub(crate) fn facts(workspace: &Path, op: &Operation) -> Vec<PathFact> {
    if !op.category.is_path_category() || op.problem.is_some() {
        return Vec::new();
    }
    let mut targets: Vec<&str> = op.paths.iter().map(String::as_str).collect();
    if op.category == OperationCategory::Search && !op.paths.iter().any(|p| !looks_like_pattern(p)) {
        targets.insert(0, "");
    }
    let canonical_root = match std::fs::canonicalize(workspace) {
        Ok(p) => p,
        Err(e) => {
            return targets
                .iter()
                .map(|_| unresolved(format!("the workspace cannot be resolved: {e}")))
                .collect();
        }
    };
    targets
        .iter()
        .map(|t| if t.is_empty() { inside_root() } else { fact(workspace, &canonical_root, t) })
        .collect()
}

fn inside_root() -> PathFact {
    PathFact::Inside { lexical: String::new(), resolved: String::new() }
}

/// Search targets added from patterns (see the Claude classifier) contain
/// glob characters; an explicit search root does not.
fn looks_like_pattern(p: &str) -> bool {
    p.contains(['*', '?', '['])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(category: OperationCategory, paths: &[&str]) -> Operation {
        Operation {
            adapter: "test".into(),
            tool: "T".into(),
            category,
            paths: paths.iter().map(|p| (*p).into()).collect(),
            command: None,
            background: false,
            unexpected_fields: Vec::new(),
            problem: None,
        }
    }

    #[test]
    fn locations_lexical_and_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let ws = root.join("ws");
        let outside = root.join("outside");
        std::fs::create_dir_all(ws.join("docs")).unwrap();
        std::fs::create_dir_all(ws.join("secrets")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("escape")).unwrap();
        std::os::unix::fs::symlink(ws.join("secrets"), ws.join("docs/link")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere/x"), ws.join("dangling")).unwrap();
        let ws_s = ws.display().to_string();
        let f = |p: &str| facts(&ws, &op(OperationCategory::FileWrite, &[p])).remove(0);

        assert_eq!(f("a/b.txt"), PathFact::Inside { lexical: "a/b.txt".into(), resolved: "a/b.txt".into() });
        assert_eq!(
            f(&format!("{ws_s}/docs/x")),
            PathFact::Inside { lexical: "docs/x".into(), resolved: "docs/x".into() }
        );
        assert_eq!(
            f("docs/../docs/./y"),
            PathFact::Inside { lexical: "docs/y".into(), resolved: "docs/y".into() }
        );
        assert_eq!(f("../outside/x"), PathFact::Outside);
        assert_eq!(f("/etc/passwd"), PathFact::Outside);
        assert_eq!(f("escape/x"), PathFact::Outside, "symlink leaving the workspace");
        assert_eq!(
            f("docs/link/key"),
            PathFact::Inside { lexical: "docs/link/key".into(), resolved: "secrets/key".into() }
        );
        assert!(matches!(f("dangling"), PathFact::Unresolved { .. }), "dangling links are not trusted");
        assert!(matches!(f("~/x"), PathFact::Unresolved { .. }));
        // Searches without a path target the workspace root; escaping
        // patterns add a target outside.
        assert_eq!(facts(&ws, &op(OperationCategory::Search, &[])), [inside_root()]);
        assert_eq!(
            facts(&ws, &op(OperationCategory::Search, &["../../**/id_rsa"])),
            [inside_root(), PathFact::Outside]
        );
        assert_eq!(facts(&ws, &op(OperationCategory::Search, &[&ws_s])).len(), 1);
        assert!(facts(&ws, &op(OperationCategory::Shell, &[])).is_empty());
    }
}
