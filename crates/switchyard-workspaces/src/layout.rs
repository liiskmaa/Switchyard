//! Names and locations. Everything on disk and in Git is derived from
//! internal UUIDs, never from user input; the display name is a label only.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Branch namespace for workspace branches: `switchyard/<workspace-id>`.
pub const BRANCH_PREFIX: &str = "switchyard/";
/// Longest display name accepted (characters).
pub const MAX_NAME_CHARS: usize = 80;
/// Longest base revision accepted (bytes).
pub const MAX_BASE_REF_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("workspace name must not be empty")]
    Empty,
    #[error("workspace name is longer than {MAX_NAME_CHARS} characters")]
    TooLong,
    #[error("workspace name must not contain control characters")]
    ControlCharacter,
    #[error("base revision {0:?} is not acceptable: {1}")]
    BaseRef(String, &'static str),
}

/// The branch a workspace owns.
pub fn branch_name(workspace_id: Uuid) -> String {
    format!("{BRANCH_PREFIX}{workspace_id}")
}

/// `<workspaces_root>/<project-id>/<workspace-id>`. `workspaces_root` is a
/// Core-owned directory outside every registered repository.
pub fn worktree_path(workspaces_root: &Path, project_id: Uuid, workspace_id: Uuid) -> PathBuf {
    workspaces_root.join(project_id.to_string()).join(workspace_id.to_string())
}

/// Trims and checks a display name.
pub fn validate_name(name: &str) -> Result<String, NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(NameError::TooLong);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter);
    }
    Ok(name.to_owned())
}

/// Checks a user-supplied base revision before it reaches `git`. Git itself
/// resolves it (after `--end-of-options`); this rejects values that could be
/// read as options or that are not plausible revisions.
pub fn validate_base_ref(rev: &str) -> Result<&str, NameError> {
    let err = |why| NameError::BaseRef(rev.to_owned(), why);
    if rev.is_empty() {
        return Err(err("empty"));
    }
    if rev.len() > MAX_BASE_REF_BYTES {
        return Err(err("too long"));
    }
    if rev.starts_with('-') {
        return Err(err("must not start with '-'"));
    }
    if rev.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(err("must not contain whitespace or control characters"));
    }
    Ok(rev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_names_use_ids_only() {
        let p = Uuid::from_u128(1);
        let w = Uuid::from_u128(2);
        assert_eq!(branch_name(w), format!("switchyard/{w}"));
        assert_eq!(
            worktree_path(Path::new("/data/work spaces"), p, w),
            PathBuf::from(format!("/data/work spaces/{p}/{w}"))
        );
    }

    #[test]
    fn names_and_refs_are_validated() {
        assert_eq!(validate_name("  Fix login ✓ "), Ok("Fix login ✓".into()));
        assert_eq!(validate_name(" "), Err(NameError::Empty));
        assert_eq!(validate_name("a\nb"), Err(NameError::ControlCharacter));
        assert_eq!(validate_name(&"x".repeat(81)), Err(NameError::TooLong));
        assert!(validate_base_ref("main").is_ok());
        assert!(validate_base_ref("origin/feature-ä").is_ok());
        assert!(validate_base_ref("--upload-pack=x").is_err());
        assert!(validate_base_ref("a b").is_err());
        assert!(validate_base_ref("").is_err());
    }
}
