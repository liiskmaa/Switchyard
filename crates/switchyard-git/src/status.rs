use std::path::Path;

use crate::command::Git;
use crate::error::GitError;
use crate::repository::accessible_directory;

/// Summary of `git status` for a working tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl GitStatus {
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.unstaged == 0 && self.untracked == 0 && self.conflicted == 0
    }
}

/// Parses `git status --porcelain=v2 --branch` output.
///
/// Only headers and the leading record markers are interpreted, so file
/// names never influence parsing.
pub fn parse_porcelain_v2(output: &str) -> Result<GitStatus, GitError> {
    let mut status = GitStatus::default();
    for line in output.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => status.head_oid = Some(value.to_owned()),
                "branch.head" if value == "(detached)" => status.detached = true,
                "branch.head" => status.branch = Some(value.to_owned()),
                "branch.upstream" => status.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    let mut parts = value.split_whitespace();
                    let parse = |p: Option<&str>, sign: char| {
                        p.and_then(|s| s.strip_prefix(sign))
                            .and_then(|s| s.parse::<u32>().ok())
                            .ok_or_else(|| GitError::UnexpectedOutput(line.to_owned()))
                    };
                    status.ahead = Some(parse(parts.next(), '+')?);
                    status.behind = Some(parse(parts.next(), '-')?);
                }
                _ => {}
            }
            continue;
        }
        match line.as_bytes() {
            [b'1' | b'2', b' ', x, y, b' ', ..] => {
                if *x != b'.' {
                    status.staged += 1;
                }
                if *y != b'.' {
                    status.unstaged += 1;
                }
            }
            [b'u', b' ', ..] => status.conflicted += 1,
            [b'?', b' ', ..] => status.untracked += 1,
            [b'!', b' ', ..] | [] => {}
            _ => return Err(GitError::UnexpectedOutput(line.to_owned())),
        }
    }
    Ok(status)
}

impl Git {
    pub async fn status(&self, root: &Path) -> Result<GitStatus, GitError> {
        let dir = accessible_directory(root).await?;
        let args = ["status", "--porcelain=v2", "--branch", "--untracked-files=normal"];
        let out = self.run(&dir, &args).await?;
        if !out.success {
            if out.stderr.contains("not a git repository") {
                return Err(GitError::NotARepository { path: dir, reason: out.stderr });
            }
            return Err(Git::failed(&args, out));
        }
        parse_porcelain_v2(&out.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_headers_and_entry_counts() {
        let out = "\
# branch.oid 0123456789abcdef0123456789abcdef01234567
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -1
1 M. N... 100644 100644 100644 aaa bbb staged.rs
1 .M N... 100644 100644 100644 aaa bbb unstaged.rs
1 MM N... 100644 100644 100644 aaa bbb both.rs
2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\told.rs
u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs
? untracked file with spaces.txt
! ignored.log
";
        let s = parse_porcelain_v2(out).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (Some(2), Some(1)));
        assert_eq!((s.staged, s.unstaged, s.untracked, s.conflicted), (3, 2, 1, 1));
        assert!(!s.detached && !s.is_clean());
    }

    #[test]
    fn parses_unborn_and_detached_heads() {
        let unborn = parse_porcelain_v2("# branch.oid (initial)\n# branch.head main\n").unwrap();
        assert_eq!((unborn.head_oid.as_deref(), unborn.branch.as_deref()), (None, Some("main")));
        assert!(unborn.is_clean());
        let detached = parse_porcelain_v2("# branch.oid abc\n# branch.head (detached)\n").unwrap();
        assert!(detached.detached && detached.branch.is_none());
    }

    #[test]
    fn rejects_unknown_record_markers() {
        assert!(parse_porcelain_v2("garbage\n").is_err());
    }
}
