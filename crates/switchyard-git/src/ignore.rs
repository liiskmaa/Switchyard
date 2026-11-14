use std::collections::HashSet;
use std::path::Path;

use crate::command::Git;
use crate::error::GitError;

impl Git {
    /// Returns the subset of `paths` (relative to `root`) that Git ignores.
    pub async fn ignored(&self, root: &Path, paths: &[String]) -> Result<HashSet<String>, GitError> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let mut input = Vec::new();
        for p in paths {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        let args = ["check-ignore", "--stdin", "-z"];
        let out = self.run_with_input(root, &args, Some(input)).await?;
        // Exit status 1 means "none of the paths are ignored".
        if !(out.success || out.code == Some(1) && out.stderr.is_empty()) {
            return Err(Git::failed(&args, out));
        }
        Ok(out.stdout.split('\0').filter(|s| !s.is_empty()).map(str::to_owned).collect())
    }
}
