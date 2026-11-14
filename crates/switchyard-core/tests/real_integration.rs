#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end acceptance test of milestone 7 with the locally
//! installed Claude Code and Codex CLIs. Claude investigates, Codex implements,
//! Claude reviews (Codex addresses findings if the review asks for changes),
//! then the operator integrates through Switchyard: commit the workspace,
//! validate through the policy, approve the exact commits, and verify Git
//! history. Real (paid) requests, so it never runs by default:
//!
//! ```sh
//! SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1 \
//!   cargo test -p switchyard-core --test real_integration -- --ignored --nocapture
//! ```
//!
//! It works in a throwaway repository. Approvals are decided as a careful
//! person would: file changes inside the workspace and read-only commands are
//! approved, everything else is denied.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use switchyard_client::CoreClient;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    Approval, ApprovalStatus, AssignmentStatus, IntegrationStatus, MessageKind, OperationCategory, Role,
    RunStatus, TaskKind, TaskStatus, WorkspaceStatus,
};
use uuid::Uuid;

use common::{RealCli, TestCore, git_repo};

async fn real_core() -> Option<TestCore> {
    TestCore::start_real(RealCli { claude: true, codex: true }, |c| {
        c.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
}

/// The simulated operator's shell policy: approve only explicitly recognized
/// read-only command forms. This is a deliberately small, strict parser for
/// this fixture, not a general shell parser: anything it does not recognize
/// is denied.
mod operator_policy {
    use std::path::{Component, Path, PathBuf};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Tok {
        Word(String),
        Op(&'static str),
    }

    /// Words and the control operators `&&`, `||`, `;`, `|`. Accepts plain
    /// words of a conservative character set, single-quoted strings, and
    /// double-quoted strings without `$`, backquote, backslash or `!`.
    /// Everything else (redirection, substitution, expansion, globbing,
    /// grouping, background `&`, comments, newlines) is refused.
    fn lex(s: &str) -> Result<Vec<Tok>, String> {
        let mut out = Vec::new();
        let mut word: Option<String> = None;
        let mut chars = s.chars().peekable();
        let flush = |word: &mut Option<String>, out: &mut Vec<Tok>| {
            if let Some(w) = word.take() {
                out.push(Tok::Word(w));
            }
        };
        while let Some(c) = chars.next() {
            match c {
                ' ' | '\t' => flush(&mut word, &mut out),
                '\'' => {
                    let w = word.get_or_insert_with(String::new);
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some('\n') | None => return Err("unterminated single quote".into()),
                            Some(ch) => w.push(ch),
                        }
                    }
                }
                '"' => {
                    let w = word.get_or_insert_with(String::new);
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some(ch @ ('$' | '`' | '\\' | '!')) => {
                                return Err(format!("{ch:?} inside double quotes (expansion)"));
                            }
                            Some('\n') | None => return Err("unterminated double quote".into()),
                            Some(ch) => w.push(ch),
                        }
                    }
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    flush(&mut word, &mut out);
                    out.push(Tok::Op("&&"));
                }
                '&' => return Err("background `&`".into()),
                '|' => {
                    flush(&mut word, &mut out);
                    if chars.peek() == Some(&'|') {
                        chars.next();
                        out.push(Tok::Op("||"));
                    } else {
                        out.push(Tok::Op("|"));
                    }
                }
                ';' => {
                    flush(&mut word, &mut out);
                    out.push(Tok::Op(";"));
                }
                c if c.is_ascii_alphanumeric() || "_./,:=@%+-".contains(c) => {
                    word.get_or_insert_with(String::new).push(c);
                }
                other => {
                    return Err(format!(
                        "unsupported shell syntax {other:?} (redirection, substitution, expansion or unknown)"
                    ));
                }
            }
        }
        flush(&mut word, &mut out);
        Ok(out)
    }

    /// What the policy knows about the session it decides for.
    pub struct Ctx {
        /// The session's workspace (canonical): every path must stay inside it.
        pub cwd: PathBuf,
        /// `rg` may run: no `RIPGREP_CONFIG_PATH` can inject options.
        pub rg_ok: bool,
        /// Git may run: no configuration, hook or environment variable that
        /// makes a read-only Git command run another program.
        pub git_ok: bool,
    }

    impl Ctx {
        pub fn for_workspace(workspace: &Path) -> Self {
            let cwd = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
            Self { rg_ok: std::env::var_os("RIPGREP_CONFIG_PATH").is_none(), git_ok: git_is_inert(&cwd), cwd }
        }
    }

    /// Whether read-only Git commands in `repo` run nothing but Git: no
    /// effective configuration that runs programs (fsmonitor, external diff,
    /// textconv/diff drivers, clean/smudge filters, a hooks path, a pager),
    /// no active hook in the shared hooks directory (`post-index-change` runs
    /// when `git status` refreshes the index), and no environment that
    /// redirects or reconfigures Git.
    pub fn git_is_inert(repo: &Path) -> bool {
        const ENV: &[&str] = &[
            "GIT_EXTERNAL_DIFF",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_EXEC_PATH",
            "GIT_PAGER",
        ];
        if ENV.iter().any(|v| std::env::var_os(v).is_some()) {
            return false;
        }
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(repo).args(args).output();
        let pattern = r"^(core\.fsmonitor|core\.hookspath|core\.pager|diff\.external|diff\..*\.(textconv|command)|filter\..*|pager\..*)$";
        match git(&["config", "--get-regexp", pattern]) {
            Ok(o) if o.status.code() == Some(1) && o.stdout.is_empty() => {}
            _ => return false,
        }
        let Ok(o) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { return false };
        if !o.status.success() {
            return false;
        }
        let hooks = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).join("hooks");
        match std::fs::read_dir(&hooks) {
            Ok(entries) => entries.flatten().all(|e| e.file_name().to_string_lossy().ends_with(".sample")),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    /// `Ok` if every command in `command` is a recognized read-only form.
    /// A shell wrapper (`/bin/zsh -lc "<script>"`, as Codex sends) is
    /// unwrapped exactly once; a wrapper inside the script is a command like
    /// any other and is not recognized.
    pub fn check(command: &str, ctx: &Ctx) -> Result<(), String> {
        let toks = lex(command)?;
        if let [Tok::Word(sh), Tok::Word(flag), Tok::Word(script)] = toks.as_slice()
            && matches!(sh.as_str(), "/bin/zsh" | "/bin/bash" | "/bin/sh" | "zsh" | "bash" | "sh")
            && matches!(flag.as_str(), "-lc" | "-c")
        {
            return check_script(lex(script)?, ctx);
        }
        check_script(toks, ctx)
    }

    fn check_script(toks: Vec<Tok>, ctx: &Ctx) -> Result<(), String> {
        let mut segment: Vec<String> = Vec::new();
        let mut segments = 0;
        for t in toks.into_iter().chain([Tok::Op("end")]) {
            match t {
                Tok::Word(w) => segment.push(w),
                Tok::Op(op) => {
                    if segment.is_empty() {
                        return Err(format!("empty command before `{op}`"));
                    }
                    check_command(&segment, ctx)?;
                    segment.clear();
                    segments += 1;
                }
            }
        }
        if segments == 0 { Err("no command".into()) } else { Ok(()) }
    }

    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }

    fn cluster(flag: &str, letters: &str) -> bool {
        flag.len() > 1
            && flag.starts_with('-')
            && !flag.starts_with("--")
            && flag[1..].chars().all(|c| letters.contains(c))
    }

    /// A path operand must stay inside the session's workspace, also after
    /// resolving symlinks: the path itself if it exists, otherwise its
    /// nearest existing ancestor. `..` is refused outright.
    fn path(p: &str, ctx: &Ctx) -> Result<(), String> {
        let path = Path::new(p);
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(format!("path {p:?} leaves the workspace"));
        }
        let full = if path.is_absolute() { path.to_path_buf() } else { ctx.cwd.join(path) };
        let mut probe = full.as_path();
        let resolved = loop {
            match std::fs::canonicalize(probe) {
                Ok(r) => break r,
                Err(_) => match probe.parent() {
                    Some(parent) => probe = parent,
                    None => return Err(format!("path {p:?} cannot be resolved")),
                },
            }
        };
        if !resolved.starts_with(&ctx.cwd) {
            return Err(format!("path {p:?} resolves outside the workspace ({})", resolved.display()));
        }
        Ok(())
    }

    /// Operands after the options: `patterns` leading pattern operands, then paths.
    fn operands(words: &[String], patterns: usize, ctx: &Ctx) -> Result<(), String> {
        for (i, w) in words.iter().enumerate() {
            if w.starts_with('-') && w != "-" {
                return Err(format!("option {w:?} after an operand"));
            }
            if i >= patterns {
                path(w, ctx)?;
            }
        }
        Ok(())
    }

    fn check_command(words: &[String], ctx: &Ctx) -> Result<(), String> {
        let (cmd, args) = words.split_first().ok_or("no command")?;
        let mut i = 0;
        // Leading options; `takes` says whether an option consumes the next word.
        let opts = |allowed: &dyn Fn(&str) -> Option<bool>, i: &mut usize| -> Result<bool, String> {
            let mut pattern_given = false;
            while *i < args.len() && args[*i].starts_with('-') && args[*i] != "-" {
                let a = &args[*i];
                if a == "--" {
                    *i += 1;
                    break;
                }
                match allowed(a) {
                    Some(true) => {
                        if *i + 1 >= args.len() {
                            return Err(format!("option {a:?} needs a value"));
                        }
                        if a == "-e" {
                            pattern_given = true;
                        }
                        *i += 2;
                    }
                    Some(false) => *i += 1,
                    None => return Err(format!("{cmd}: option {a:?} is not recognized as read-only")),
                }
            }
            Ok(pattern_given)
        };
        match cmd.as_str() {
            "pwd" if args.is_empty() => Ok(()),
            "ls" => {
                opts(&|a| cluster(a, "lahR1AFt").then_some(false), &mut i)?;
                operands(&args[i..], 0, ctx)
            }
            "cat" => {
                opts(&|a| (a == "-n").then_some(false), &mut i)?;
                operands(&args[i..], 0, ctx)
            }
            "head" | "tail" => {
                let f = |a: &str| {
                    if a == "-n" {
                        Some(true)
                    } else if a.strip_prefix("-n").is_some_and(digits)
                        || a.strip_prefix('-').is_some_and(digits)
                    {
                        Some(false)
                    } else {
                        None
                    }
                };
                opts(&f, &mut i)?;
                if args.windows(2).any(|w| w[0] == "-n" && !digits(&w[1])) {
                    return Err("-n needs a number".into());
                }
                operands(&args[i..], 0, ctx)
            }
            "wc" => {
                opts(&|a| cluster(a, "lwcm").then_some(false), &mut i)?;
                operands(&args[i..], 0, ctx)
            }
            "nl" => {
                opts(&|a| (a == "-ba").then_some(false), &mut i)?;
                operands(&args[i..], 0, ctx)
            }
            "grep" => {
                let f = |a: &str| {
                    if a == "-e" {
                        Some(true)
                    } else if cluster(a, "nrilcEFwvHhsxo")
                        || ["--include=", "--exclude=", "--exclude-dir="].iter().any(|p| a.starts_with(p))
                        || a == "--color=never"
                    {
                        Some(false)
                    } else {
                        None
                    }
                };
                let pattern = opts(&f, &mut i)?;
                operands(&args[i..], usize::from(!pattern), ctx)
            }
            "rg" if !ctx.rg_ok => Err("rg: RIPGREP_CONFIG_PATH could add options; not approved".into()),
            "rg" => {
                let f = |a: &str| {
                    if ["-e", "-g", "--glob", "-t", "--type"].contains(&a) {
                        Some(true)
                    } else if cluster(a, "nilcFwvHSuo")
                        || ["--files", "--hidden", "--no-heading", "--line-number", "--color=never"]
                            .contains(&a)
                        || a.starts_with("--glob=")
                        || a.starts_with("--type=")
                    {
                        Some(false)
                    } else {
                        None
                    }
                };
                let pattern = opts(&f, &mut i)?;
                let files = args.iter().any(|a| a == "--files");
                operands(&args[i..], usize::from(!pattern && !files), ctx)
            }
            "find" => {
                // Starting points, then only these tests: no actions (-exec,
                // -execdir, -ok, -okdir, -delete, -fprint*, -fls, ...), no
                // symlink following (-L, -H, -follow).
                while i < args.len() && !args[i].starts_with('-') {
                    path(&args[i], ctx)?;
                    i += 1;
                }
                while i < args.len() {
                    let value = args.get(i + 1).map(String::as_str);
                    match args[i].as_str() {
                        "-name" | "-iname" | "-path" | "-ipath" if value.is_some() => i += 2,
                        "-maxdepth" | "-mindepth" if value.is_some_and(digits) => i += 2,
                        "-type" if matches!(value, Some("f" | "d")) => i += 2,
                        "-print" | "-not" | "-o" | "-a" => i += 1,
                        other => return Err(format!("find: {other:?} is not recognized as read-only")),
                    }
                }
                Ok(())
            }
            "sed" => {
                // Only `sed -n 'A,Bp' [paths]`: printing line ranges.
                let script_ok = |s: &str| {
                    let Some(body) = s.strip_suffix('p') else { return false };
                    let mut parts = body.splitn(2, ',');
                    let first = parts.next().unwrap_or("");
                    digits(first) && parts.next().is_none_or(|second| digits(second) || second == "$")
                };
                match args {
                    [n, script, rest @ ..] if n == "-n" && script_ok(script) => operands(rest, 0, ctx),
                    _ => Err("sed: only `sed -n 'A,Bp' FILE…` is recognized".into()),
                }
            }
            "git" if !ctx.git_ok => {
                Err("git: this repository's configuration, hooks or environment could run programs".into())
            }
            "git" => {
                let (sub, rest) = args.split_first().ok_or("git: no subcommand")?;
                let log_like = |a: &str| {
                    if a == "-n" {
                        Some(true)
                    } else if [
                        "--stat",
                        "--name-only",
                        "--name-status",
                        "--oneline",
                        "-p",
                        "--patch",
                        "--no-color",
                        "--cached",
                        "--staged",
                        "--no-ext-diff",
                        "--graph",
                        "--decorate",
                        "--all",
                        "--no-patch",
                    ]
                    .contains(&a)
                        || a.strip_prefix('-').is_some_and(digits)
                        || a.strip_prefix("-U").is_some_and(digits)
                        || a.strip_prefix("--unified=").is_some_and(digits)
                        || a.strip_prefix("--max-count=").is_some_and(digits)
                        || a.starts_with("--format=")
                        || a.starts_with("--pretty=")
                    {
                        Some(false)
                    } else {
                        None
                    }
                };
                let status = |a: &str| {
                    [
                        "-s",
                        "--short",
                        "-b",
                        "--branch",
                        "-sb",
                        "--porcelain",
                        "--porcelain=v1",
                        "--porcelain=v2",
                        "--ignored",
                        "--untracked-files=all",
                        "--untracked-files=no",
                        "--untracked-files=normal",
                    ]
                    .contains(&a)
                    .then_some(false)
                };
                let ls_files = |a: &str| {
                    [
                        "--others",
                        "--exclude-standard",
                        "-o",
                        "-c",
                        "--cached",
                        "-m",
                        "--modified",
                        "-s",
                        "--stage",
                    ]
                    .contains(&a)
                    .then_some(false)
                };
                let allowed: &dyn Fn(&str) -> Option<bool> = match sub.as_str() {
                    "status" => &status,
                    "diff" | "log" | "show" => &log_like,
                    "ls-files" => &ls_files,
                    other => return Err(format!("git {other} is not recognized as read-only")),
                };
                let mut j = 0;
                while j < rest.len() && rest[j].starts_with('-') {
                    let a = &rest[j];
                    if a == "--" {
                        j += 1;
                        break;
                    }
                    match allowed(a) {
                        Some(true) if j + 1 < rest.len() && digits(&rest[j + 1]) => j += 2,
                        Some(false) => j += 1,
                        _ => return Err(format!("git {sub}: option {a:?} is not recognized as read-only")),
                    }
                }
                // Revisions and paths: no options, nothing outside the workspaces.
                operands(&rest[j..], 0, ctx)
            }
            other => Err(format!("{other:?} is not a recognized read-only command")),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A real workspace directory with a file, a subdirectory, and
        /// symlinks that lead out of it.
        struct Fixture {
            _dir: tempfile::TempDir,
            ctx: Ctx,
        }

        fn fixture() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let ws = dir.path().join("ws");
            std::fs::create_dir_all(ws.join("src")).unwrap();
            std::fs::write(ws.join("durations.py"), "x\n").unwrap();
            std::fs::write(dir.path().join("outside.txt"), "secret\n").unwrap();
            std::os::unix::fs::symlink(dir.path().join("outside.txt"), ws.join("out.txt")).unwrap();
            std::os::unix::fs::symlink(dir.path(), ws.join("up")).unwrap();
            std::os::unix::fs::symlink(ws.join("durations.py"), ws.join("inside-link.py")).unwrap();
            let git = |args: &[&str]| {
                let st = std::process::Command::new("git").arg("-C").arg(&ws).args(args).status().unwrap();
                assert!(st.success());
            };
            git(&["init", "-q"]);
            let ctx =
                Ctx { rg_ok: true, git_ok: git_is_inert(&ws), cwd: std::fs::canonicalize(&ws).unwrap() };
            Fixture { _dir: dir, ctx }
        }

        fn ok(f: &Fixture, c: &str) {
            assert_eq!(check(c, &f.ctx), Ok(()), "should be approved: {c}");
        }
        fn denied(f: &Fixture, c: &str) {
            let r = check(c, &f.ctx);
            assert!(r.is_err(), "should be denied: {c}");
        }

        #[test]
        fn harmless_chained_reads_are_approved() {
            let f = fixture();
            assert!(f.ctx.git_ok, "a fresh repository is inert");
            // The exact command Codex was denied in the first paid run.
            ok(&f, "/bin/zsh -lc \"pwd && rg --files -g 'durations.py' -g 'AGENTS.md'\"");
            ok(&f, "ls -la; git ls-files; cat durations.py");
            ok(&f, "cat durations.py | head -n 20");
            ok(&f, "git diff --stat && git status --short");
            ok(&f, "git log --oneline -5 || git status");
            ok(&f, "ls|cat");
            ok(&f, "ls&&pwd");
            ok(&f, "sed -n '1,200p' durations.py");
            ok(&f, "sed -n '5,$p' durations.py");
            ok(&f, "nl -ba durations.py | sed -n '1,40p'");
            ok(&f, "find . -maxdepth 2 -type f -name '*.py'");
            ok(&f, "wc -l durations.py && pwd");
            ok(&f, "cat src/new_file.py");
            ok(&f, "cat inside-link.py");
            let abs = format!("cat {}/durations.py", f.ctx.cwd.display());
            ok(&f, &abs);
        }

        #[test]
        fn quoted_arguments_are_literal() {
            let f = fixture();
            ok(&f, "grep -n 'raise ValueError' durations.py");
            ok(&f, "rg -n \"def parse\" .");
            ok(&f, "rg --files -g '*.py'");
            ok(&f, "grep -e '$HOME' durations.py");
            ok(&f, "grep -rn '1h30m' .");
            ok(&f, "git log --format='%h %s' -n 3");
            ok(&f, "grep -n '' durations.py");
            // Operators inside quotes are literal pattern text, not chains.
            ok(&f, "grep -n '&&' durations.py");
            ok(&f, "grep -n \"a;b|c\" durations.py");
            ok(&f, "grep -n '$(rm -rf .)' durations.py");
        }

        #[test]
        fn redirection_is_denied() {
            let f = fixture();
            denied(&f, "cat durations.py > copy.py");
            denied(&f, "ls tests 2>/dev/null");
            // Claude's exact command in the first paid run: still denied (redirections).
            denied(&f, "ls -la; git ls-files; cat durations.py 2>/dev/null; ls tests 2>/dev/null");
            denied(&f, "cat < durations.py");
            denied(&f, "ls >> log");
            denied(&f, "ls >| log");
            denied(&f, "/bin/zsh -lc \"cat a > b\"");
            denied(&f, "cat durations.py | tee copy.py");
        }

        #[test]
        fn substitution_and_expansion_are_denied() {
            let f = fixture();
            denied(&f, "cat $(echo durations.py)");
            denied(&f, "cat `whoami`");
            denied(&f, "cat \"$HOME/x\"");
            denied(&f, "cat \"`id`\"");
            denied(&f, "ls ${HOME}");
            denied(&f, "ls *.py");
            denied(&f, "cat ~/secret");
            denied(&f, "ls ?");
            denied(&f, "ls [ab]");
            denied(&f, "cat \"a\\\"b\"");
            denied(&f, "echo \"!!\"");
            denied(&f, "cat <(ls)");
            denied(&f, "/bin/zsh -lc \"cat $(id)\"");
        }

        #[test]
        fn find_cannot_run_commands_or_write() {
            let f = fixture();
            for action in [
                "-exec rm {} ;",
                "-exec cat '{}' ';'",
                "-execdir cat '{}' ';'",
                "-ok cat '{}' ';'",
                "-okdir cat '{}' ';'",
                "-delete",
                "-fprint out",
                "-fprint0 out",
                "-fprintf out '%p'",
                "-fls out",
                "-ls",
                "-printf '%p'",
                "-prune",
                "-newer durations.py",
            ] {
                denied(&f, &format!("find . -name '*.py' {action}"));
            }
            denied(&f, "find -L . -name x");
            denied(&f, "find -H . -name x");
            denied(&f, "find . -follow -name x");
            denied(&f, "find . -maxdepth x");
            denied(&f, "find . -type l");
            denied(&f, "find up -name x");
            ok(&f, "find . -name '*.py' -print");
            ok(&f, "find src -not -name x -o -iname y");
        }

        #[test]
        fn options_with_side_effects_are_denied() {
            let f = fixture();
            for c in [
                // grep
                "grep -R x .",
                "grep -f patterns durations.py",
                "grep --exclude-from=list x .",
                "grep -d recurse x .",
                "grep -S x .",
                "grep -O x .",
                "grep -P x durations.py",
                // sed
                "sed -i 's/a/b/' durations.py",
                "sed -n 'w out' durations.py",
                "sed -n '1e date' durations.py",
                "sed -n '1,5p;w x' durations.py",
                "sed -n -e '1p' durations.py",
                "sed -E -n '1p' durations.py",
                "sed 's/a/b/' durations.py",
                // head / tail
                "tail -f durations.py",
                "tail -F durations.py",
                "tail -r durations.py",
                "tail -n +3 durations.py",
                "head -c 10 durations.py",
                // rg
                "rg --pre=sh x",
                "rg --pre sh x",
                "rg -L x",
                "rg --follow x",
                "rg -z x",
                "rg --search-zip x",
                "rg --type-add 'x:*.py' x",
                "rg --ignore-file=list x",
                // git
                "git -c core.pager=sh log",
                "git -C .. status",
                "git --no-pager log",
                "git --git-dir=x log",
                "git diff --output=x",
                "git log --output=x",
                "git diff --ext-diff",
                "git show --textconv HEAD",
                "git diff --no-index a b",
                "git log --follow durations.py",
                "git config --list",
                "git branch -D x",
                "git push",
                "git commit -m x",
                "git checkout durations.py",
                "git stash",
                "git status --porcelain=v3",
                // other commands
                "python3 -c 'print(1)'",
                "rm -rf .",
                "touch a",
                "curl https://example.invalid",
                "xargs cat",
                "env ls",
                "exec ls",
                "pwd -P",
                "ls -la; rm durations.py",
                "cat durations.py | python3",
            ] {
                denied(&f, c);
            }
        }

        #[test]
        fn rg_and_git_are_denied_when_their_environment_could_run_programs() {
            let f = fixture();
            let no_rg = Ctx { cwd: f.ctx.cwd.clone(), rg_ok: false, git_ok: true };
            assert!(check("rg -n x", &no_rg).is_err());
            assert!(check("grep -n x durations.py", &no_rg).is_ok());
            let git = |args: &[&str]| {
                let st =
                    std::process::Command::new("git").arg("-C").arg(&f.ctx.cwd).args(args).status().unwrap();
                assert!(st.success());
            };
            for (key, value) in [
                ("core.fsmonitor", "touch /tmp/x"),
                ("diff.external", "sh"),
                ("diff.py.textconv", "sh"),
                ("filter.lfs.clean", "sh"),
                ("core.hooksPath", "hooks"),
                ("core.pager", "sh"),
            ] {
                git(&["config", key, value]);
                assert!(!git_is_inert(&f.ctx.cwd), "{key} makes Git run programs");
                git(&["config", "--unset", key]);
                assert!(git_is_inert(&f.ctx.cwd));
            }
            let hook = f.ctx.cwd.join(".git/hooks/post-index-change");
            std::fs::write(&hook, "#!/bin/sh\n").unwrap();
            assert!(!git_is_inert(&f.ctx.cwd), "an active hook");
            std::fs::remove_file(hook).unwrap();
            assert!(git_is_inert(&f.ctx.cwd));
            let unsafe_git = Ctx { cwd: f.ctx.cwd.clone(), rg_ok: true, git_ok: false };
            assert!(check("git status", &unsafe_git).is_err());
        }

        #[test]
        fn symlinks_and_paths_outside_the_workspace_are_denied() {
            let f = fixture();
            denied(&f, "cat out.txt");
            denied(&f, "ls up");
            denied(&f, "cat up/outside.txt");
            denied(&f, "grep -rn x up");
            denied(&f, "rg x up");
            denied(&f, "cat up/does-not-exist");
            denied(&f, "cat ../secret");
            denied(&f, "cat /etc/passwd");
            denied(&f, "grep -rn x /Users");
            let via_parent = format!("ls {}/../outside.txt", f.ctx.cwd.display());
            denied(&f, &via_parent);
        }

        #[test]
        fn the_shell_wrapper_is_unwrapped_exactly_once() {
            let f = fixture();
            ok(&f, "/bin/zsh -lc 'ls'");
            ok(&f, "bash -c \"git status\"");
            denied(&f, "/bin/zsh -lc \"/bin/zsh -lc ls\"");
            denied(&f, "/bin/zsh -lc \"bash -c 'ls'\"");
            denied(&f, "/bin/zsh -lc 'sh -c ls'");
            denied(&f, "/bin/zsh -lc \"ls\" extra");
            denied(&f, "/bin/zsh -x -lc ls");
            denied(&f, "/bin/zsh -lc");
            denied(&f, "/usr/local/bin/zsh -lc ls");
            denied(&f, "zsh ls");
        }

        /// The critical denials happen for the intended reason, not by accident.
        #[test]
        fn critical_denials_have_the_intended_reason() {
            let f = fixture();
            let why = |c: &str| check(c, &f.ctx).unwrap_err();
            for (command, reason) in [
                ("find . -name x -exec cat '{}' ';'", "find: \"-exec\""),
                ("find . -execdir cat '{}' ';'", "find: \"-execdir\""),
                ("find . -ok cat '{}' ';'", "find: \"-ok\""),
                ("find . -okdir cat '{}' ';'", "find: \"-okdir\""),
                ("find . -delete", "find: \"-delete\""),
                ("find . -fprint out", "find: \"-fprint\""),
                ("find . -fprintf out x", "find: \"-fprintf\""),
                ("find . -fls out", "find: \"-fls\""),
                ("find -L . -name x", "find: \"-L\""),
                ("grep -R x .", "grep: option \"-R\""),
                ("rg -L x", "rg: option \"-L\""),
                ("rg --pre=sh x", "rg: option \"--pre=sh\""),
                ("sed -n 'w out' durations.py", "sed: only"),
                ("tail -f durations.py", "tail: option \"-f\""),
                ("git diff --ext-diff", "git diff: option \"--ext-diff\""),
                ("git diff --no-index a b", "git diff: option \"--no-index\""),
                ("git -c core.pager=sh log", "git -c is not recognized"),
                ("cat out.txt", "resolves outside the workspace"),
                ("cat up/outside.txt", "resolves outside the workspace"),
                ("/bin/zsh -lc \"bash -c 'ls'\"", "\"bash\" is not a recognized read-only command"),
                ("cat durations.py > x", "redirection"),
                ("cat $(id)", "redirection, substitution"),
                ("ls &", "background"),
            ] {
                let r = why(command);
                assert!(r.contains(reason), "{command}: {r}");
            }
        }

        #[test]
        fn malformed_syntax_is_denied() {
            let f = fixture();
            for c in [
                "",
                "   ",
                "cat 'unterminated",
                "cat \"unterminated",
                "ls &&",
                "&& ls",
                "ls ;",
                "; ls",
                "ls ;; cat a",
                "ls; ; cat a",
                "ls &",
                "ls & & cat",
                "ls | | cat",
                "ls |",
                "ls ||| cat",
                "ls\ncat a",
                "(ls)",
                "{ ls; }",
                "ls # comment",
                "head -n x durations.py",
                "head -n",
                r"ls \; cat",
            ] {
                denied(&f, c);
            }
        }
    }
}

/// The simulated operator: file reads and edits inside the workspace and
/// recognized read-only commands ([`operator_policy`]) are approved;
/// everything else is denied.
fn decide(a: &Approval, ctx: &operator_policy::Ctx) -> ApprovalDecision {
    let category = a.policy.as_ref().map(|p| p.category);
    let inside = a.policy.as_ref().and_then(|p| p.scope.as_ref()).is_some_and(|s| s.kind == "workspace");
    match category {
        Some(OperationCategory::FileWrite | OperationCategory::FileEdit | OperationCategory::FileRead)
            if inside =>
        {
            ApprovalDecision::Approve
        }
        Some(OperationCategory::Shell) => {
            match a.input["command"].as_str().map(|c| operator_policy::check(c, ctx)) {
                Some(Ok(())) => ApprovalDecision::Approve,
                Some(Err(why)) => {
                    println!("operator denies: {why}");
                    ApprovalDecision::Deny
                }
                None => ApprovalDecision::Deny,
            }
        }
        _ => ApprovalDecision::Deny,
    }
}

/// The operator policy for a session in workspace `ws`.
async fn policy_for(c: &CoreClient, ws: Uuid) -> operator_policy::Ctx {
    let path = c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: ws }).await.unwrap().workspace.path;
    let ctx = operator_policy::Ctx::for_workspace(Path::new(&path));
    println!(
        "operator policy for {}: rg allowed {}, git allowed {}",
        ctx.cwd.display(),
        ctx.rg_ok,
        ctx.git_ok
    );
    ctx
}

/// How long one agent phase may take before acceptance ends as not passed.
const PHASE_DEADLINE: Duration = Duration::from_secs(600);
/// Report-repair nudges allowed per task.
const MAX_NUDGES: usize = 2;
/// How long Core may take to stop a session after its report (it stops it
/// itself, asynchronously, once the report is committed) before acceptance
/// ends as not passed.
const SESSION_STOP_DEADLINE: Duration = Duration::from_secs(120);

/// Waits until `session` is terminal, no live session is attached to
/// workspace `ws`, and Core has released the workspace from `active`: the
/// preconditions Core checks before any integration step in that workspace
/// (`begin_integration_op`: no `live_session`, and a status that may become
/// `integrating`). Core stops a reported assignment's session on its own
/// after the report is committed, so the task can already read as reported
/// while the session is still exiting; and it releases the workspace in a
/// later step than the session's terminal state (`WorkspaceService::release`
/// inspects the worktree first), so `active` can briefly outlive the
/// session. Every operator action that needs the workspace must come after
/// this. Polls Core's state; on `deadline` the acceptance ends as not passed
/// with the complete evidence.
async fn await_sessions_stopped(
    c: &CoreClient,
    ws: Uuid,
    session: Uuid,
    ev: &Evidence,
    deadline: Duration,
) -> switchyard_protocol::model::Session {
    let started = tokio::time::Instant::now();
    let until = started + deadline;
    let mut first_state = None;
    // When the session was first seen terminal with nothing live attached.
    let mut ended_at: Option<tokio::time::Instant> = None;
    loop {
        let s = c.call::<SessionsGet>(SessionIdParams { session_id: session }).await.unwrap().session;
        first_state.get_or_insert(s.state);
        let w = c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: ws }).await.unwrap();
        let live: Vec<(Uuid, String)> =
            w.sessions.iter().filter(|w| w.state.is_live()).map(|w| (w.id, w.state.to_string())).collect();
        let ended = s.state.is_terminal() && live.is_empty();
        if ended {
            ended_at.get_or_insert_with(tokio::time::Instant::now);
        }
        let released = w.workspace.status != WorkspaceStatus::Active;
        if ended && released {
            let waited = started.elapsed().as_millis() as u64;
            let release_lag = ended_at.map(|t| t.elapsed().as_millis() as u64);
            ev.record(
                "session_stops",
                serde_json::json!({
                    "phase": ev.phase(), "session": session, "workspace": ws,
                    "state_when_reported": first_state.map(|f| f.to_string()), "final_state": s.state.to_string(),
                    "exit_code": s.exit_code, "workspace_status": w.workspace.status.to_string(),
                    "waited_ms": waited, "workspace_active_after_session_ended_ms": release_lag
                }),
            );
            ev.log(&format!(
                "session {session} is {} (was {} when the report was seen); workspace {ws} is {} with no live \
                 session; waited {waited} ms (workspace still active for {release_lag:?} ms after the session ended)",
                s.state,
                first_state.map(|f| f.to_string()).unwrap_or_default(),
                w.workspace.status
            ));
            return s;
        }
        if tokio::time::Instant::now() >= until {
            ev.not_passed(&format!(
                "session stop: in phase {} the session {session} was still {}, the workspace's live sessions were \
                 {live:?} and the workspace was {} {} s after the report; the next operator action was not attempted",
                ev.phase(),
                s.state,
                w.workspace.status,
                deadline.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The session of the latest `role` assignment in `d`.
fn session_of_role(d: &TaskDetail, role: Role) -> Uuid {
    d.assignments
        .iter()
        .filter(|a| a.role == role)
        .max_by_key(|a| a.created_at_ms)
        .and_then(|a| a.session_id)
        .unwrap_or_else(|| panic!("no {role:?} session in {:?}", d.assignments))
}

/// `(invalid value, allowed values)` from a serde "unknown variant" error,
/// e.g. "unknown variant `minor`, expected one of `info`, `low`, ...".
fn invalid_variant(error: &str) -> Option<(String, Vec<String>)> {
    let rest = &error[error.find("unknown variant `")? + "unknown variant `".len()..];
    let value = rest[..rest.find('`')?].to_owned();
    let expected = &rest[rest.find("expected one of ")? + "expected one of ".len()..];
    let allowed: Vec<String> = expected.split('`').skip(1).step_by(2).map(str::to_owned).collect();
    (!allowed.is_empty()).then_some((value, allowed))
}

/// The operator's repair request after Core rejected a report: Core's exact
/// reason, the invalid value and the allowed values when Core names them,
/// and a request for the complete corrected report with everything else
/// unchanged. It never suggests what the findings should be.
fn repair_note(error: &str) -> String {
    let mut note = format!("Switchyard rejected your switchyard-report block: {error}.");
    if let Some((value, allowed)) = invalid_variant(error) {
        note.push_str(&format!(
            " The value `{value}` is not allowed there; use exactly one of: {}.",
            allowed.join(", ")
        ));
    }
    note.push_str(
        " Send the complete corrected switchyard-report block again at the end of your message, keeping all of \
         your findings and acceptance results unchanged except for what this error names.",
    );
    note
}

async fn drive(
    c: &CoreClient,
    task: Uuid,
    working: TaskStatus,
    ctx: &operator_policy::Ctx,
    ev: &Evidence,
    deadline: Duration,
) -> TaskDetail {
    let until = tokio::time::Instant::now() + deadline;
    loop {
        let d = c.call::<TasksGet>(TaskIdParams { task_id: task }).await.unwrap();
        // Every message, once: rejections, nudges and the reports that follow.
        for m in &d.messages {
            if ev.first_sight(m.id) {
                ev.record(
                    "messages",
                    serde_json::json!({
                        "phase": ev.phase(), "id": m.id, "kind": m.kind, "sender": m.sender, "payload": m.payload
                    }),
                );
            }
        }
        if d.task.status != working && d.task.status != TaskStatus::Assigned {
            return d;
        }
        if let Some(s) = d.task.open_assignment.as_ref().and_then(|a| a.session_id) {
            let pending = c
                .call::<ApprovalsList>(ApprovalsListParams {
                    status: Some(ApprovalStatus::Pending),
                    session_id: Some(s),
                    ..Default::default()
                })
                .await
                .unwrap()
                .approvals;
            for a in pending {
                let decision = decide(&a, ctx);
                ev.log(&format!(
                    "approval {} {:?} {} -> {decision:?}",
                    a.tool_name,
                    a.policy.as_ref().map(|p| p.category),
                    a.input
                ));
                let _ = c
                    .call::<ApprovalsDecide>(ApprovalsDecideParams {
                        approval_id: a.id,
                        decision,
                        note: None,
                    })
                    .await;
            }
        }
        if let Some(m) = d.messages.last()
            && m.kind == MessageKind::MalformedReport
        {
            let error = m.payload["error"].as_str().unwrap_or("the report could not be read").to_owned();
            let used = d.messages.iter().filter(|m| m.kind == MessageKind::OperatorNote).count();
            if used < MAX_NUDGES {
                let note = repair_note(&error);
                ev.record(
                    "repairs",
                    serde_json::json!({
                        "phase": ev.phase(), "rejected_message": m.id, "error": error,
                        "excerpt": m.payload["excerpt"], "nudge": used + 1, "text": note
                    }),
                );
                ev.log(&format!("report rejected ({error}); nudge {} of {MAX_NUDGES}", used + 1));
                c.call::<TasksNote>(TasksNoteParams { task_id: task, text: note }).await.unwrap();
            } else {
                ev.record(
                    "repairs",
                    serde_json::json!({
                        "phase": ev.phase(), "rejected_message": m.id, "error": error,
                        "excerpt": m.payload["excerpt"], "nudge": null, "exhausted": true
                    }),
                );
                ev.not_passed(&format!(
                    "report repair exhausted in phase {}: {MAX_NUDGES} nudges were used and the report was rejected \
                     again: {error}",
                    ev.phase()
                ));
            }
        }
        if tokio::time::Instant::now() >= until {
            ev.not_passed(&format!(
                "deadline: phase {} was still {working} after {} s (task {task}, status {}, reason {:?})",
                ev.phase(),
                deadline.as_secs(),
                d.task.status,
                d.task.status_reason
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn assign(c: &CoreClient, task: Uuid, role: Role, provider: &str, ws: Uuid) {
    c.call::<TasksAssign>(TasksAssignParams {
        task_id: task,
        role,
        provider: provider.into(),
        workspace_id: ws,
        start: true,
        model: None,
    })
    .await
    .unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn agents_build_and_review_then_the_operator_integrates() {
    let Some(core) = real_core().await else { return };
    let (c, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "real-integration");
    git(&repo, &["config", "user.name", "Switchyard Acceptance"]);
    git(&repo, &["config", "user.email", "acceptance@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("greet.py"), "def greet(name):\n    return 'Hello ' + name\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "__pycache__/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "main"]);
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    let ws = c
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    let check = c
        .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: project.id,
            command_id: None,
            name: "greeting check".into(),
            argv: vec![
                "/usr/bin/python3".into(),
                "-B".into(),
                "-c".into(),
                "import greet; assert greet.greet('world') == 'Hello world!', greet.greet('world')".into(),
            ],
            timeout_secs: 60,
            enabled: true,
            position: None,
        })
        .await
        .unwrap()
        .commands[0]
        .id;
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project.id,
            title: "Make greet() add an exclamation mark".into(),
            description: Some("greet('world') must return 'Hello world!'. Change only greet.py and keep it small. Do not run Python.".into()),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec!["greet('world') returns 'Hello world!'".into(), "only greet.py changes".into()],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task;

    // 1-4. Investigate, implement, review; fix and review again if changes are requested.
    let ev = Evidence::new(core.dir.path());
    let ctx = policy_for(&c, ws.id).await;
    assign(&c, task.id, Role::Investigate, "claude_code", ws.id).await;
    assert_eq!(
        drive(&c, task.id, TaskStatus::InProgress, &ctx, &ev, PHASE_DEADLINE).await.task.status,
        TaskStatus::AwaitingReview
    );
    let mut fix_rounds = 0;
    loop {
        assign(&c, task.id, Role::Implement, "codex", ws.id).await;
        assert_eq!(
            drive(&c, task.id, TaskStatus::InProgress, &ctx, &ev, PHASE_DEADLINE).await.task.status,
            TaskStatus::AwaitingReview
        );
        assign(&c, task.id, Role::Review, "claude_code", ws.id).await;
        let d = drive(&c, task.id, TaskStatus::InReview, &ctx, &ev, PHASE_DEADLINE).await;
        println!("review: {:?}", d.task.status_reason);
        if d.task.status_reason.as_deref() == Some("review: approved") || fix_rounds == 1 {
            assert_eq!(
                d.task.status_reason.as_deref(),
                Some("review: approved"),
                "the review did not approve"
            );
            break;
        }
        fix_rounds += 1;
    }
    println!("fix rounds after review: {fix_rounds}");
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "agents never touched the target");
    // Core stops the reviewer's session after its report; wait for that.
    let d = c.call::<TasksGet>(TaskIdParams { task_id: task.id }).await.unwrap();
    await_sessions_stopped(&c, ws.id, session_of_role(&d, Role::Review), &ev, SESSION_STOP_DEADLINE).await;

    // 5. The operator creates the integration.
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: Some(task.id),
            workspace_id: None,
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    println!("prepared: {:?} {:?}", i.status, i.blockers);
    // 6. Commit the agent's work; inspect the actual diff and commit identities.
    let i = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Make greet() add an exclamation mark".into(),
        })
        .await
        .unwrap()
        .integration;
    let ch = c.call::<IntegrationsChanges>(IntegrationsChangesParams { integration_id: i.id }).await.unwrap();
    println!(
        "changes: {} commit(s), files {:?}",
        ch.commits_total,
        ch.files.iter().map(|f| (&f.path, &f.kind)).collect::<Vec<_>>()
    );
    assert!(ch.files.iter().any(|f| f.path == "greet.py"));
    assert_eq!(
        (ch.source_commit.as_str(), ch.target_commit.as_str()),
        (i.source_commit.as_str(), base.as_str())
    );
    let patch = c
        .call::<IntegrationsPatch>(IntegrationsPatchParams {
            integration_id: i.id,
            path: Some("greet.py".into()),
            uncommitted: false,
        })
        .await
        .unwrap();
    println!("patch:\n{}", patch.patch);
    // 7. Validation through the policy (it asks; the operator authorizes).
    let plan = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().plan;
    assert_eq!(plan[0].policy_decision, "ask");
    c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: plan
            .iter()
            .filter(|p| p.command_id == check)
            .map(|p| ValidationAuthorization {
                command_id: p.command_id,
                command_line: p.command_line.clone(),
            })
            .collect(),
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let d = loop {
        let d = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
        if d.integration.status != IntegrationStatus::Validating
            && d.runs.first().is_some_and(|r| r.status != RunStatus::Running)
        {
            break d;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    println!(
        "validation: {:?} {:?}",
        d.runs[0].status,
        d.runs[0].steps.iter().map(|s| (&s.name, s.status, &s.stderr)).collect::<Vec<_>>()
    );
    assert_eq!(d.runs[0].status, RunStatus::Passed);
    // 8. Conflicts checked: ready means the source contains the target.
    assert_eq!(d.integration.status, IntegrationStatus::Ready, "{:?}", d.integration.blockers);
    // 9-10. The operator approves the exact commits; Core integrates.
    let done = c
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: d.integration.source_commit.clone(),
            target_commit: d.integration.target_commit.clone(),
        })
        .await
        .unwrap()
        .integration;
    assert_eq!(done.status, IntegrationStatus::Integrated, "{:?}", done.failure);
    // 11. The journal has the whole operation.
    let events: Vec<String> = c
        .call::<EventsList>(EventsListParams {
            project_id: Some(project.id),
            after_seq: Some(0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .filter(|e| e.payload["integration_id"] == serde_json::json!(i.id))
        .map(|e| e.event_type)
        .collect();
    println!("integration events: {events:?}");
    for e in [
        "integration.created",
        "integration.workspace_changed",
        "integration.validation_finished",
        "integration.approved",
        "integration.integrated",
    ] {
        assert!(events.contains(&e.to_owned()), "{e}");
    }
    // 12. Git history and delivery status.
    let merge = done.merge_commit.unwrap();
    assert_eq!(git(&repo, &["rev-parse", "main"]), merge);
    println!("{}", git(&repo, &["log", "--graph", "--oneline", "-5", "main"]));
    println!("{}", git(&repo, &["log", "-1", "--format=%B", "main"]));
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%P", "main"]),
        format!("{base} {}", d.integration.source_commit)
    );
    assert!(std::fs::read_to_string(repo.join("greet.py")).unwrap().contains('!'));
    let t = c.call::<TasksGet>(TaskIdParams { task_id: task.id }).await.unwrap();
    assert_eq!(t.task.status, TaskStatus::AwaitingReview, "delivery is not completion");
    let completed = c
        .call::<TasksComplete>(TaskDecisionParams { task_id: task.id, note: Some("integrated".into()) })
        .await
        .unwrap();
    assert_eq!(completed.task.status, TaskStatus::Completed);
}

// --- Final acceptance: review → fix → review, validation evidence, Core-owned
// operations across a client disconnect ---------------------------------------

/// A Git wrapper (inside the test's own directory) that runs the hooks in
/// `dir` (`hook-NAME`: a pattern; `hook-NAME.sh`: a shell action) before
/// the real Git. Transparent without hooks; used only to slow one Git step
/// down so a client can disconnect while it runs.
fn git_wrapper(dir: &Path) -> std::path::PathBuf {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let path = dir.join("git");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nfor f in '{d}'/hook-*; do\n  case \"$f\" in *.sh) continue ;; esac\n  [ -f \"$f\" ] || continue\n  \
             pat=$(head -n 1 \"$f\")\n  case \"$*\" in *\"$pat\"*) sh \"$f.sh\" ;; esac\ndone\nexec '{real}' \"$@\"\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

const DURATIONS_SPEC: &str = "Implement parse_duration(text) in durations.py. It returns the number of seconds in a \
     duration written as one or more <integer><unit> parts with the units h, m and s, each unit at most once and in \
     that order: '1h30m' -> 5400, '45s' -> 45, '2m5s' -> 125, '1h0m7s' -> 3607. Whitespace around the whole text is \
     ignored. It raises ValueError for an empty or blank string, an unknown unit, a part without a number, a repeated \
     or out-of-order unit, and any other text. Change only durations.py and keep it small. Do not run Python.";

const DURATIONS_CHECK: &str = "from durations import parse_duration as p\n\
cases = {'1h30m': 5400, '45s': 45, '2m5s': 125, '1h0m7s': 3607, ' 3m ': 180}\n\
for text, want in cases.items():\n    assert p(text) == want, (text, p(text), want)\n\
for bad in ['', '   ', '5', 'm', '5x', '30m1h', '1h1h', '1h 30m', '-5s', 'h5']:\n    try:\n        p(bad)\n    except ValueError:\n        continue\n    raise AssertionError('accepted ' + repr(bad))\n\
print('validated parse_duration on', len(cases), 'valid and 10 invalid inputs')";

/// A defective implementation for the controlled review fixture: it accepts
/// repeated and out-of-order units ('30m1h', '1h1h'), which the
/// specification forbids. A genuine defect; the reviewer is not told about it.
const DEFECTIVE_DURATIONS: &str = concat!(
    "import re\n",
    "\n",
    "_UNITS = {'h': 3600, 'm': 60, 's': 1}\n",
    "\n",
    "\n",
    "def parse_duration(text):\n",
    "    text = text.strip()\n",
    "    if not text:\n",
    "        raise ValueError('empty duration')\n",
    "    parts = re.findall(r'(\\d+)([hms])', text)\n",
    "    if ''.join(n + u for n, u in parts) != text:\n",
    "        raise ValueError('invalid duration: ' + text)\n",
    "    return sum(int(n) * _UNITS[u] for n, u in parts)\n",
);

/// A correct implementation, for the free dry run of the operator half.
const CORRECT_DURATIONS: &str = concat!(
    "import re\n",
    "\n",
    "_FORM = re.compile(r'(?:(\\d+)h)?(?:(\\d+)m)?(?:(\\d+)s)?')\n",
    "\n",
    "\n",
    "def parse_duration(text):\n",
    "    text = text.strip()\n",
    "    m = _FORM.fullmatch(text)\n",
    "    if not text or not m:\n",
    "        raise ValueError('invalid duration: ' + repr(text))\n",
    "    h, mi, s = (int(x) if x else 0 for x in m.groups())\n",
    "    return h * 3600 + mi * 60 + s\n",
);

struct Review {
    rounds: Vec<Option<String>>,
}

thread_local! {
    /// The last panic message on this thread, for the evidence of an
    /// unexpected panic (recorded by [`install_panic_recorder`]).
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn install_panic_recorder() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let message = info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| info.payload().downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "non-string panic".into());
            let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
            LAST_PANIC.with(|p| *p.borrow_mut() = Some(format!("{message}{at}")));
            previous(info);
        }));
    });
}

/// The acceptance evidence, in `<dir>/evidence/` of the disposable Core
/// directory. Nothing here deletes anything. Every way a run ends goes
/// through [`Evidence::not_passed`] or [`Evidence::passed`]; an unexpected
/// panic is caught when the `Evidence` is dropped during unwinding. Writing
/// evidence never panics (failures are noted in `evidence/errors.log`).
struct Evidence {
    dir: PathBuf,
    /// The evidence folder's name inside `dir` (`evidence` unless named).
    name: String,
    started: std::time::SystemTime,
    phase: std::sync::Mutex<String>,
    repo: std::sync::Mutex<Option<PathBuf>>,
    finished: std::sync::atomic::AtomicBool,
    /// Messages already recorded (each once, under the phase it appeared in).
    seen: std::sync::Mutex<std::collections::HashSet<Uuid>>,
}

impl Evidence {
    fn new(dir: &Path) -> Self {
        Self::named(dir, "evidence")
    }

    /// Evidence in `<dir>/<name>/`, for several experiments in one Core.
    fn named(dir: &Path, name: &str) -> Self {
        install_panic_recorder();
        let _ = std::fs::create_dir_all(dir.join(name));
        Self {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            started: std::time::SystemTime::now(),
            phase: std::sync::Mutex::new("setup".into()),
            repo: std::sync::Mutex::new(None),
            finished: std::sync::atomic::AtomicBool::new(false),
            seen: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    fn first_sight(&self, message: Uuid) -> bool {
        self.seen.lock().map(|mut s| s.insert(message)).unwrap_or(true)
    }

    fn root(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    fn phase(&self) -> String {
        self.phase.lock().map(|p| p.clone()).unwrap_or_default()
    }

    fn set_phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.log(&format!("phase: {phase}"));
        if let Ok(mut p) = self.phase.lock() {
            *p = phase;
        }
    }

    fn set_repo(&self, repo: &Path) {
        if let Ok(mut r) = self.repo.lock() {
            *r = Some(repo.to_path_buf());
        }
    }

    fn append(&self, file: &str, line: &str) {
        use std::io::Write;
        let r = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root().join(file))
            .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
        if let Err(e) = r {
            eprintln!("evidence: cannot append to {file}: {e}");
        }
    }

    fn note_error(&self, what: &str, e: impl std::fmt::Display) {
        self.append("errors.log", &format!("{what}: {e}"));
    }

    /// Prints `line` and keeps it in `evidence/run.log`.
    fn log(&self, line: &str) {
        println!("{line}");
        let ms = self.started.elapsed().map(|d| d.as_millis()).unwrap_or(0);
        self.append("run.log", &format!("[+{ms} ms] {line}"));
    }

    /// Appends one JSON line to `evidence/<stream>.jsonl`.
    fn record(&self, stream: &str, value: serde_json::Value) {
        self.append(&format!("{stream}.jsonl"), &value.to_string());
    }

    /// Writes a complete snapshot to `evidence/<label>/`: Core's records,
    /// each workspace's Git status/diff/HEAD, the repository's refs and log,
    /// and copies of the agents' own transcripts. Returns the snapshot dir.
    fn write(&self, label: &str) -> PathBuf {
        let out = self.root().join(label);
        if let Err(e) = std::fs::create_dir_all(&out) {
            self.note_error("snapshot dir", e);
        }
        if let Err(e) = self.write_records(&out.join("core-records.json")) {
            self.note_error("core records", e);
        }
        self.write_workspaces(&out.join("workspaces"));
        self.write_git(&out.join("git"));
        self.copy_transcripts(&out.join("transcripts"));
        println!("evidence written: {}", out.display());
        out
    }

    fn write_records(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let db = rusqlite::Connection::open_with_flags(
            self.dir.join("data/switchyard.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut all = serde_json::Map::new();
        for table in [
            "events",
            "tasks",
            "task_assignments",
            "messages",
            "sessions",
            "approvals",
            "workspaces",
            "integrations",
            "integration_operations",
            "validation_runs",
            "validation_steps",
        ] {
            let mut st = db.prepare(&format!("SELECT * FROM {table}"))?;
            let names: Vec<String> = st.column_names().map(str::to_owned).collect();
            let rows = st
                .query_map([], |r| {
                    let mut m = serde_json::Map::new();
                    for (i, n) in names.iter().enumerate() {
                        let v = match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => x.into(),
                            rusqlite::types::ValueRef::Real(x) => x.into(),
                            rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                                let s = String::from_utf8_lossy(t).into_owned();
                                serde_json::from_str::<serde_json::Value>(&s)
                                    .ok()
                                    .filter(|v| v.is_object() || v.is_array())
                                    .unwrap_or(serde_json::Value::String(s))
                            }
                        };
                        m.insert(n.clone(), v);
                    }
                    Ok(serde_json::Value::Object(m))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            all.insert(table.into(), rows.into());
        }
        std::fs::write(path, serde_json::to_string_pretty(&serde_json::Value::Object(all))?)?;
        Ok(())
    }

    /// Read-only Git output (`--no-optional-locks`: nothing is written).
    fn git_out(dir: &Path, args: &[&str]) -> String {
        match Command::new("git").arg("--no-optional-locks").arg("-C").arg(dir).args(args).output() {
            Ok(o) => format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
            Err(e) => format!("cannot run git: {e}"),
        }
    }

    fn write_workspaces(&self, out: &Path) {
        let _ = std::fs::create_dir_all(out);
        let Ok(projects) = std::fs::read_dir(self.dir.join("data/workspaces")) else { return };
        for project in projects.flatten() {
            let Ok(workspaces) = std::fs::read_dir(project.path()) else { continue };
            for ws in workspaces.flatten() {
                let name = ws.file_name().to_string_lossy().into_owned();
                let path = ws.path();
                for (ext, args) in [
                    ("status", &["status", "--porcelain=v1", "--branch"][..]),
                    ("diff", &["diff", "HEAD"][..]),
                    ("head", &["log", "-3", "--format=%H %P %s"][..]),
                ] {
                    if let Err(e) =
                        std::fs::write(out.join(format!("{name}.{ext}")), Self::git_out(&path, args))
                    {
                        self.note_error("workspace", e);
                    }
                }
            }
        }
    }

    fn write_git(&self, out: &Path) {
        let Some(repo) = self.repo.lock().ok().and_then(|r| r.clone()) else { return };
        let _ = std::fs::create_dir_all(out);
        for (file, args) in [
            ("refs.txt", &["for-each-ref", "--format=%(refname) %(objectname)"][..]),
            ("log.txt", &["log", "--all", "--graph", "--format=%H %P %s"][..]),
            ("status.txt", &["status", "--porcelain=v1", "--branch"][..]),
            ("worktrees.txt", &["worktree", "list", "--porcelain"][..]),
        ] {
            if let Err(e) = std::fs::write(out.join(file), Self::git_out(&repo, args)) {
                self.note_error("git", e);
            }
        }
    }

    /// Copies the transcripts the CLIs keep for sessions in this directory
    /// (Claude: `~/.claude/projects/<escaped cwd>`; Codex: rollouts under
    /// `~/.codex/sessions` written since this run started that mention it).
    fn copy_transcripts(&self, out: &Path) {
        let _ = std::fs::create_dir_all(out.join("claude"));
        let _ = std::fs::create_dir_all(out.join("codex"));
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        let key = self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut found = Vec::new();
        if let Ok(projects) = std::fs::read_dir(home.join(".claude/projects")) {
            for p in projects
                .flatten()
                .filter(|p| !key.is_empty() && p.file_name().to_string_lossy().contains(&key))
            {
                for f in std::fs::read_dir(p.path()).into_iter().flatten().flatten() {
                    let target = out.join("claude").join(f.file_name());
                    if std::fs::copy(f.path(), &target).is_ok() {
                        found.push(target.display().to_string());
                    }
                }
            }
        }
        let mut stack = vec![home.join(".codex/sessions")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let recent = e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m >= self.started);
                if recent && std::fs::read_to_string(&path).is_ok_and(|t| !key.is_empty() && t.contains(&key))
                {
                    let target = out.join("codex").join(e.file_name());
                    if std::fs::copy(&path, &target).is_ok() {
                        found.push(target.display().to_string());
                    }
                }
            }
        }
        let _ = std::fs::write(out.join("manifest.txt"), found.join("\n"));
    }

    fn conclude(&self, outcome: &str, reason: &str, snapshot: &Path) {
        let ended = std::time::SystemTime::now();
        let ms = |t: std::time::SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
        };
        let t = serde_json::json!({
            "outcome": outcome,
            "phase": self.phase(),
            "reason": reason,
            "phase_deadline_secs": PHASE_DEADLINE.as_secs(),
            "max_nudges": MAX_NUDGES,
            "started_at_ms": ms(self.started),
            "ended_at_ms": ms(ended),
            "snapshot": snapshot.display().to_string(),
        });
        let _ = std::fs::write(
            self.root().join("termination.json"),
            serde_json::to_string_pretty(&t).unwrap_or_default(),
        );
        let _ = std::fs::write(self.root().join("OUTCOME.txt"), format!("ACCEPTANCE {outcome}: {reason}\n"));
        self.log(&format!("ACCEPTANCE {outcome} in phase {}: {reason}", self.phase()));
    }

    /// Records the outcome with a complete snapshot and fails the test:
    /// acceptance did not pass.
    fn not_passed(&self, what: &str) -> ! {
        self.finished.store(true, std::sync::atomic::Ordering::SeqCst);
        let what = what.trim_end_matches('.');
        let snapshot = self.write("not-passed");
        self.conclude("NOT PASSED", what, &snapshot);
        panic!("ACCEPTANCE NOT PASSED: {what}. Evidence: {}", self.root().display());
    }

    fn passed(&self) {
        self.finished.store(true, std::sync::atomic::Ordering::SeqCst);
        let snapshot = self.write("final");
        self.conclude("PASSED", "every acceptance step completed", &snapshot);
    }
}

impl Drop for Evidence {
    fn drop(&mut self) {
        if std::thread::panicking() && !self.finished.load(std::sync::atomic::Ordering::SeqCst) {
            let reason = LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "unknown panic".into());
            let snapshot = self.write("panic");
            self.conclude("NOT PASSED", &format!("unexpected panic: {reason}"), &snapshot);
        }
    }
}

/// Assigns `role` to `provider` and drives the task until the agent's
/// report, then waits until Core has stopped that session (so the next
/// operator action never races Core's own stop). A blocker report, or any
/// other outcome than a report, is recorded as such (with the agent's own
/// words) and ends the test as not passed.
#[allow(clippy::too_many_arguments)]
async fn run_phase(
    c: &CoreClient,
    task: Uuid,
    role: Role,
    provider: &str,
    ws: Uuid,
    ctx: &operator_policy::Ctx,
    ev: &Evidence,
) -> TaskDetail {
    ev.set_phase(format!("{role:?} by {provider}"));
    assign(c, task, role, provider, ws).await;
    let working = if role == Role::Review { TaskStatus::InReview } else { TaskStatus::InProgress };
    let d = drive(c, task, working, ctx, ev, PHASE_DEADLINE).await;
    let last =
        |kind: MessageKind| d.messages.iter().rev().find(|m| m.kind == kind).map(|m| m.payload.clone());
    match d.task.status {
        TaskStatus::AwaitingReview => {
            await_sessions_stopped(c, ws, session_of_role(&d, role), ev, SESSION_STOP_DEADLINE).await;
            d
        }
        TaskStatus::Blocked => {
            let report = last(MessageKind::Blocker);
            println!("BLOCKER from {role:?} ({provider}): {report:?}");
            let summary = report
                .as_ref()
                .and_then(|r| r["report"]["summary"].as_str().or(r["summary"].as_str()).map(str::to_owned))
                .unwrap_or_else(|| d.task.status_reason.clone().unwrap_or_default());
            ev.not_passed(&format!("the {role:?} agent ({provider}) reported a blocker: {summary}"))
        }
        other => ev.not_passed(&format!(
            "the {role:?} agent ({provider}) left the task {other} ({:?})",
            d.task.status_reason
        )),
    }
}

/// Implement (Codex) then review (Claude); one fix round at most if the
/// review asks for changes. Returns every review verdict, in order.
async fn implement_and_review(
    c: &CoreClient,
    task: Uuid,
    ws: Uuid,
    implement_first: bool,
    ctx: &operator_policy::Ctx,
    ev: &Evidence,
) -> Review {
    let mut rounds = Vec::new();
    if implement_first {
        run_phase(c, task, Role::Implement, "codex", ws, ctx, ev).await;
    }
    for round in 0..2 {
        let d = run_phase(c, task, Role::Review, "claude_code", ws, ctx, ev).await;
        let verdict = d.task.status_reason.clone();
        println!("review round {}: {verdict:?}", round + 1);
        if let Some(m) = d.messages.iter().rev().find(|m| m.kind == MessageKind::ReviewResult) {
            println!("review report: {}", m.payload);
        }
        let approved = verdict.as_deref() == Some("review: approved");
        rounds.push(verdict);
        if approved || round == 1 {
            break;
        }
        // `run_phase` has waited for Core to stop the reviewer's session.
        run_phase(c, task, Role::Implement, "codex", ws, ctx, ev).await;
    }
    Review { rounds }
}

/// Everything after the agents: commit, inspect, validate (with evidence),
/// approve across a client disconnect, and check Git, the journal, the
/// operation records and a Core restart. No model calls.
#[allow(clippy::too_many_arguments)]
async fn operator_integrates(
    core: &mut TestCore,
    hooks: &Path,
    repo: &Path,
    project: Uuid,
    task: Option<Uuid>,
    workspace: Uuid,
    check: Uuid,
    base: &str,
) {
    let (c, _rx) = core.connect().await;
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: task,
            workspace_id: if task.is_some() { None } else { Some(workspace) },
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    let i = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Add parse_duration".into(),
        })
        .await
        .unwrap()
        .integration;
    let ch = c.call::<IntegrationsChanges>(IntegrationsChangesParams { integration_id: i.id }).await.unwrap();
    println!("changes: {:?}", ch.files.iter().map(|f| (&f.path, &f.kind)).collect::<Vec<_>>());
    assert_eq!(ch.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["durations.py"]);
    let patch = c
        .call::<IntegrationsPatch>(IntegrationsPatchParams {
            integration_id: i.id,
            path: Some("durations.py".into()),
            uncommitted: false,
        })
        .await
        .unwrap();
    println!("patch:\n{}", patch.patch);

    // Validation: the policy asks; the operator authorizes the exact line shown.
    let plan = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().plan;
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].policy_decision, "ask");
    let shown = plan[0].command_line.clone();
    c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: vec![ValidationAuthorization { command_id: check, command_line: shown.clone() }],
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let d = loop {
        let d = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
        if d.integration.status != IntegrationStatus::Validating
            && d.runs.first().is_some_and(|r| r.status != RunStatus::Running)
        {
            break d;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let run = &d.runs[0];
    let step = &run.steps[0];
    println!(
        "validation run {} {:?}: step {:?} line={} timeout={} decision={} authorized_by={:?} exit={:?} \
         started={:?} ended={:?}\nstdout: {:?}\nstderr: {:?}",
        run.id,
        run.status,
        step.status,
        step.command_line,
        step.timeout_secs,
        step.policy_decision,
        step.authorized_by,
        step.exit_code,
        step.started_at_ms,
        step.ended_at_ms,
        step.stdout,
        step.stderr
    );
    assert_eq!(run.status, RunStatus::Passed);
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.source_commit, d.integration.source_commit);
    assert_eq!(
        (step.command_line.as_str(), step.timeout_secs, step.policy_decision.as_str()),
        (shown.as_str(), 60, "ask")
    );
    assert!(
        step.authorized_by.as_deref().is_some_and(|a| a != "policy" && !a.is_empty()),
        "{:?}",
        step.authorized_by
    );
    assert_eq!(step.exit_code, Some(0));
    assert!(step.started_at_ms.is_some() && step.ended_at_ms.is_some(), "the command actually ran");
    assert!(
        step.stdout.as_deref().is_some_and(|o| o.contains("validated parse_duration")),
        "{:?}",
        step.stdout
    );
    // The recorded configuration snapshot is exactly what ran.
    let db = rusqlite::Connection::open(core.dir.path().join("data/switchyard.db")).unwrap();
    let config: String = db
        .query_row("SELECT config FROM validation_runs WHERE id = ?1", [run.id.to_string()], |r| r.get(0))
        .unwrap();
    println!("recorded configuration: {config:?}");
    assert_eq!(config, format!("{check}\t{shown}\t60\task"));
    assert_eq!(d.integration.status, IntegrationStatus::Ready, "{:?}", d.integration.blockers);

    // Approve on a second connection, slowed down inside the checkout
    // fast-forward, and disconnect that client while Git runs.
    std::fs::write(hooks.join("hook-ff"), "merge --ff-only\n").unwrap();
    std::fs::write(hooks.join("hook-ff.sh"), "sleep 6").unwrap();
    let (c2, _rx2) = core.connect().await;
    let p = IntegrationsApproveParams {
        integration_id: i.id,
        source_commit: d.integration.source_commit.clone(),
        target_commit: d.integration.target_commit.clone(),
    };
    let request = tokio::spawn(async move { c2.call::<IntegrationsApprove>(p).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let running = loop {
        let x = c
            .call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id })
            .await
            .unwrap()
            .integration;
        if x.status == IntegrationStatus::Integrating
            && x.operation.as_ref().is_some_and(|o| o.kind == "approve" && o.status == "running")
        {
            break x;
        }
        assert!(tokio::time::Instant::now() < deadline, "approval never started: {x:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let op = running.operation.clone().unwrap();
    println!("operation recorded before the Git step: {op:?}");
    // The Git child is Core's (this process runs Core): wait until the
    // fast-forward itself has started.
    let me = std::process::id().to_string();
    let ff_child = || {
        let ps = String::from_utf8(
            Command::new("ps").args(["-Ao", "pid=,ppid=,command="]).output().unwrap().stdout,
        )
        .unwrap();
        ps.lines()
            .find(|l| l.contains("merge --ff-only") && l.split_whitespace().nth(1) == Some(me.as_str()))
            .map(str::to_owned)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let child = loop {
        if let Some(line) = ff_child() {
            break line;
        }
        assert!(tokio::time::Instant::now() < deadline, "the fast-forward never ran as Core's child");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    println!("git child before the disconnect: {child}");
    request.abort();
    let _ = request.await;
    println!("client disconnected while Git was running");
    let still = ff_child();
    println!("git child after the disconnect: {still:?}");
    assert!(still.is_some(), "the disconnect did not kill Git");
    // A new client sees the same operation; a repeated approval is refused.
    let (c3, _rx3) = core.connect().await;
    let seen =
        c3.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().integration;
    assert_eq!(seen.operation.as_ref().map(|o| (o.id, o.status.as_str())), Some((op.id, "running")));
    let again = c3
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: d.integration.source_commit.clone(),
            target_commit: d.integration.target_commit.clone(),
        })
        .await;
    println!("repeated approval while running: {again:?}");
    assert!(again.is_err());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let done = loop {
        let x = c3
            .call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id })
            .await
            .unwrap()
            .integration;
        if x.operation.as_ref().is_some_and(|o| o.id == op.id && o.status != "running") {
            break x;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let _ = std::fs::remove_file(hooks.join("hook-ff"));
    println!("after reconnecting: {:?} {:?} {:?}", done.status, done.operation, done.status_reason);
    assert_eq!(done.status, IntegrationStatus::Integrated, "{:?}", done.failure);
    assert_eq!(done.operation.as_ref().map(|o| o.status.as_str()), Some("completed"));
    let again = c3
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: d.integration.source_commit.clone(),
            target_commit: d.integration.target_commit.clone(),
        })
        .await;
    assert!(again.is_err(), "already integrated");

    // Git state matches the recorded outcome; exactly one integration merge.
    let merge = done.merge_commit.clone().unwrap();
    assert_eq!(git(repo, &["rev-parse", "main"]), merge);
    assert_eq!(
        git(repo, &["log", "-1", "--format=%P", "main"]),
        format!("{base} {}", d.integration.source_commit)
    );
    let merges = git(
        repo,
        &["log", "--merges", "--format=%H", &format!("--grep=Switchyard-Integration: {}", i.id), "main"],
    );
    assert_eq!(merges.lines().count(), 1);
    assert_eq!(git(repo, &["status", "--porcelain"]), "", "the checkout is clean at the merge");
    println!("{}", git(repo, &["log", "--graph", "--format=%h %p %s", "-6", "main"]));
    println!("{}", git(repo, &["log", "-1", "--format=%B", "main"]));
    // Locks released: the workspace is no longer `integrating`, no operation runs.
    let w = c3.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: workspace }).await.unwrap().workspace;
    assert_ne!(w.status, switchyard_protocol::model::WorkspaceStatus::Integrating);
    let ops: Vec<(String, String, String)> = {
        let mut st = db
            .prepare("SELECT kind, status, requested_by FROM integration_operations WHERE integration_id = ?1 ORDER BY started_at")
            .unwrap();
        st.query_map([i.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    println!("operations: {ops:?}");
    assert!(ops.iter().all(|(_, s, _)| s == "completed"), "{ops:?}");
    assert_eq!(ops.iter().filter(|(k, _, _)| k == "approve").count(), 1, "approved once");
    let events: Vec<String> = all_events(&c3, project)
        .await
        .into_iter()
        .filter(|e| e.payload["integration_id"] == serde_json::json!(i.id))
        .map(|e| e.event_type)
        .collect();
    println!("integration events: {events:?}");
    assert_eq!(events.iter().filter(|e| *e == "integration.integrated").count(), 1);
    assert_eq!(events.iter().filter(|e| *e == "integration.approved").count(), 1);
    drop((c, c3, db));

    // The record survives a restart of this disposable Core unchanged.
    let wrapper = hooks.join("git");
    core.restart_with(move |cfg| {
        cfg.agent_ready_timeout = Duration::from_secs(90);
        cfg.git.binary = wrapper;
    })
    .await;
    let (c, _rx) = core.connect().await;
    let after =
        c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().integration;
    println!("after a Core restart: {:?} {:?}", after.status, after.operation);
    assert_eq!(after.status, IntegrationStatus::Integrated);
    assert_eq!(after.operation.map(|o| (o.id, o.status)), Some((op.id, "completed".to_owned())));
    assert_eq!(git(repo, &["rev-parse", "main"]), merge);
}

/// Sets up a disposable repository with `durations.py` and the validation command.
async fn durations_project(core: &TestCore, c: &CoreClient) -> (std::path::PathBuf, Uuid, Uuid, String) {
    let repo = git_repo(core.dir.path(), "acceptance");
    git(&repo, &["config", "user.name", "Switchyard Acceptance"]);
    git(&repo, &["config", "user.email", "acceptance@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("durations.py"), "def parse_duration(text):\n    raise NotImplementedError\n")
        .unwrap();
    std::fs::write(repo.join(".gitignore"), "__pycache__/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "main"]);
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    let check = c
        .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: project.id,
            command_id: None,
            name: "duration check".into(),
            argv: vec!["/usr/bin/python3".into(), "-B".into(), "-c".into(), DURATIONS_CHECK.into()],
            timeout_secs: 60,
            enabled: true,
            position: None,
        })
        .await
        .unwrap()
        .commands[0]
        .id;
    (repo, project.id, check, base)
}

async fn new_workspace(c: &CoreClient, project: Uuid) -> Uuid {
    c.call::<WorkspacesCreate>(WorkspacesCreateParams { project_id: project, name: None, base_ref: None })
        .await
        .unwrap()
        .workspace
        .id
}

async fn durations_task(c: &CoreClient, project: Uuid, title: &str) -> Uuid {
    c.call::<TasksCreate>(TasksCreateParams {
        project_id: project,
        title: title.into(),
        description: Some(DURATIONS_SPEC.into()),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec![
            "valid durations return the right number of seconds".into(),
            "every invalid form listed in the description raises ValueError".into(),
            "only durations.py changes".into(),
        ],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    })
    .await
    .unwrap()
    .task
    .id
}

/// Every event of a project, in pages of 1000.
async fn all_events(c: &CoreClient, project: Uuid) -> Vec<switchyard_protocol::EventEnvelope> {
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = c
            .call::<EventsList>(EventsListParams {
                project_id: Some(project),
                after_seq: Some(after),
                limit: Some(1000),
                ..Default::default()
            })
            .await
            .unwrap()
            .events;
        let n = page.len();
        if let Some(last) = page.last() {
            after = last.seq;
        }
        out.extend(page);
        if n < 1000 {
            return out;
        }
    }
}

/// Claude's and Codex's spend, from the journal.
async fn spend(c: &CoreClient, project: Uuid) {
    let events = all_events(c, project).await;
    let turns: Vec<_> = events.iter().filter(|e| e.event_type == "agent.turn_completed").collect();
    let usd: f64 = turns.iter().filter_map(|e| e.payload["turn_cost_usd"].as_f64()).sum();
    let tokens: i64 = turns.iter().filter_map(|e| e.payload["usage"]["total_tokens"].as_i64()).sum();
    let sessions = events.iter().filter(|e| e.event_type == "agent.started").count();
    println!(
        "spend: {sessions} sessions, {} completed turns, reported Claude cost ${usd:.4}, reported tokens {tokens}",
        turns.len()
    );
}

/// Whether the final acceptance also runs the controlled defective fixture
/// when the first review approves. Off: the authorized final flow is the
/// natural one only (investigate, implement, review, at most one correction).
const RUN_DEFECTIVE_FIXTURE: bool = false;

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn final_acceptance_review_fix_review_and_core_owned_operations() {
    let mut hooks = tempfile::Builder::new().prefix("syh").tempdir().unwrap();
    let wrapper = git_wrapper(hooks.path());
    let Some(mut core) = TestCore::start_real(RealCli { claude: true, codex: true }, |cfg| {
        cfg.agent_ready_timeout = Duration::from_secs(90);
        cfg.git.binary = wrapper;
    })
    .await
    else {
        return;
    };
    // Evidence is kept on success and on failure: never deleted by the test.
    core.dir.disable_cleanup(true);
    hooks.disable_cleanup(true);
    let ev = Evidence::new(core.dir.path());
    ev.log(&format!("evidence directory (kept): {}", ev.dir.display()));
    let (c, _rx) = core.connect().await;
    let (repo, project, check, base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);

    // 1-6. The natural flow: Claude investigates, Codex implements, Claude reviews;
    // one fix round if the review asks for changes.
    let ws = new_workspace(&c, project).await;
    let task = durations_task(&c, project, "Parse durations like 1h30m").await;
    let ctx = policy_for(&c, ws).await;
    run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;
    let natural = implement_and_review(&c, task, ws, true, &ctx, &ev).await;
    println!("NATURAL FLOW verdicts: {:?}", natural.rounds);
    if natural.rounds.last().unwrap().as_deref() != Some("review: approved") {
        ev.not_passed(&format!("the natural flow's final review did not approve: {:?}", natural.rounds));
    }
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "agents never touched the target");

    // Only if the first review approved: a controlled fixture for the review loop.
    // A separate task and workspace hold a genuinely defective implementation
    // (repeated / out-of-order units accepted); nobody tells the reviewer.
    if RUN_DEFECTIVE_FIXTURE && natural.rounds.len() == 1 {
        let fx_ws = new_workspace(&c, project).await;
        let ws_path = std::path::PathBuf::from(
            c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: fx_ws }).await.unwrap().workspace.path,
        );
        std::fs::write(ws_path.join("durations.py"), DEFECTIVE_DURATIONS).unwrap();
        let fx = durations_task(&c, project, "Parse durations like 1h30m (review fixture)").await;
        let fx_ctx = policy_for(&c, fx_ws).await;
        run_phase(&c, fx, Role::Investigate, "claude_code", fx_ws, &fx_ctx, &ev).await;
        let fixture = implement_and_review(&c, fx, fx_ws, false, &fx_ctx, &ev).await;
        println!("FIXTURE verdicts: {:?}", fixture.rounds);
        let fixed = std::fs::read_to_string(ws_path.join("durations.py")).unwrap();
        println!("fixture after the fix round:\n{fixed}");
        if fixture.rounds.len() != 2 {
            ev.not_passed("the reviewer approved the deliberately defective implementation");
        }
        if fixture.rounds[1].as_deref() != Some("review: approved") {
            ev.not_passed(&format!("the fix was not approved on re-review: {:?}", fixture.rounds));
        }
    }
    // `run_phase` waited for Core to stop every reported session.
    spend(&c, project).await;
    ev.write("agents");
    drop(c);
    ev.set_phase("operator: validate, approve across a disconnect, restart");

    // 7-10. The operator integrates the natural task (no further model calls).
    operator_integrates(&mut core, hooks.path(), &repo, project, Some(task), ws, check, &base).await;
    ev.passed();
}

/// The operator half of the acceptance test with no agent at all, so a test
/// bug cannot waste a paid run. Runs by default.
#[tokio::test]
async fn acceptance_operator_half_without_agents() {
    let hooks = tempfile::Builder::new().prefix("syh").tempdir().unwrap();
    let wrapper = git_wrapper(hooks.path());
    let mut core = TestCore::start_with(|cfg| cfg.git.binary = wrapper).await;
    // Kept only on request (this test runs in the normal suite).
    let keep = std::env::var("SWITCHYARD_KEEP_EVIDENCE").as_deref() == Ok("1");
    let mut hooks = hooks;
    if keep {
        core.dir.disable_cleanup(true);
        hooks.disable_cleanup(true);
        println!("evidence directory (kept): {}", core.dir.path().display());
    }
    let (c, _rx) = core.connect().await;
    let (repo, project, check, base) = durations_project(&core, &c).await;
    c.call::<IntegrationsSetSettings>(IntegrationsSetSettingsParams {
        project_id: project,
        require_review: false,
    })
    .await
    .unwrap();
    let ws = new_workspace(&c, project).await;
    let path = std::path::PathBuf::from(
        c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: ws }).await.unwrap().workspace.path,
    );
    std::fs::write(path.join("durations.py"), CORRECT_DURATIONS).unwrap();
    drop(c);
    operator_integrates(&mut core, hooks.path(), &repo, project, None, ws, check, &base).await;
    if keep {
        Evidence::new(core.dir.path()).passed();
    }
}

/// An implementer's blocker report is recorded as a blocker, with evidence,
/// and the acceptance test ends as not passed (fake Codex, no model calls).
#[tokio::test]
async fn a_blocker_report_is_recorded_and_ends_acceptance_as_not_passed() {
    let core = TestCore::start_with(|_| {}).await;
    let (c, _rx) = core.connect().await;
    let (_repo, project, _check, _base) = durations_project(&core, &c).await;
    let ws = new_workspace(&c, project).await;
    let spec = serde_json::json!({
        "implement": { "report": { "type": "blocker", "summary": "Workspace inspection was rejected." } }
    });
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project,
            title: "Blocked implementation".into(),
            description: Some(format!("{DURATIONS_SPEC} [fake:report:{hex}]")),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec![],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id;
    let ev_dir = core.dir.path().to_path_buf();
    let ctx = policy_for(&c, ws).await;
    let phase = tokio::spawn(async move {
        let ev = Evidence::new(&ev_dir);
        run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    });
    let err = phase.await.unwrap_err();
    let message =
        err.into_panic().downcast::<String>().map(|m| *m).unwrap_or_else(|_| "non-string panic".into());
    println!("{message}");
    assert!(
        message.starts_with(
            "ACCEPTANCE NOT PASSED: the Implement agent (codex) reported a blocker: Workspace inspection was rejected. Evidence:"
        ),
        "{message}"
    );
    let outcome = std::fs::read_to_string(core.dir.path().join("evidence/OUTCOME.txt")).unwrap();
    assert!(outcome.contains("reported a blocker"), "{outcome}");
    let evidence: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(core.dir.path().join("evidence/not-passed/core-records.json")).unwrap(),
    )
    .unwrap();
    assert!(
        evidence["messages"].as_array().unwrap().iter().any(|m| m["kind"] == "blocker"),
        "the blocker report is in the evidence"
    );
    assert!(evidence["tasks"].as_array().unwrap().iter().any(|t| t["status"] == "blocked"));
}

// --- Harness tests with fake agents (no model calls) ------------------------------

const ALLOWED: &str = "info, low, medium, high, critical";

#[test]
fn a_repair_note_names_the_invalid_value_and_the_allowed_values() {
    let error = "the report is not valid: unknown variant `minor`, expected one of `info`, `low`, `medium`, `high`, \
                 `critical` at line 1 column 52";
    assert_eq!(
        invalid_variant(error),
        Some(("minor".into(), ["info", "low", "medium", "high", "critical"].map(String::from).to_vec()))
    );
    let note = repair_note(error);
    assert!(note.contains(error), "{note}");
    assert!(note.contains("`minor` is not allowed"), "{note}");
    assert!(note.contains(&format!("use exactly one of: {ALLOWED}.")), "{note}");
    assert!(note.contains("keeping all of your findings"), "{note}");
    // Other rejections are forwarded verbatim, without invented values.
    let other = repair_note("the turn ended without a switchyard-report block");
    assert!(
        other.contains("without a switchyard-report block") && !other.contains("exactly one of"),
        "{other}"
    );
}

/// A review report with one finding of `severity`.
fn review_report(severity: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "review",
        "summary": "One problem",
        "result": "changes_requested",
        "findings": [{
            "severity": severity,
            "title": "isdigit() accepts non-ASCII digits",
            "path": "durations.py",
            "line": 13,
            "explanation": "'١h' parses as 3600",
            "suggestion": "accept only ASCII digits"
        }],
        "acceptance": [{ "criterion": "valid durations return the right number of seconds", "met": true }]
    })
}

/// A disposable Core with fake agents, a task whose implementation has been
/// reported, and a review scenario of `review_steps`.
struct Harness {
    core: TestCore,
    c: CoreClient,
    task: Uuid,
    ws: Uuid,
    ctx: operator_policy::Ctx,
}

async fn harness(
    review_steps: serde_json::Value,
    ev_of: impl FnOnce(&TestCore) -> Evidence,
) -> (Harness, Evidence) {
    let core = TestCore::start_with(|_| {}).await;
    let ev = ev_of(&core);
    let (c, _rx) = core.connect().await;
    let (repo, project, _check, _base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);
    let ws = new_workspace(&c, project).await;
    let spec = serde_json::json!({
        "implement": { "report": { "type": "implementation", "summary": "Done." } },
        "review": review_steps,
    });
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project,
            title: "Harness".into(),
            description: Some(format!("{DURATIONS_SPEC} [fake:report:{hex}]")),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec![],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id;
    let ctx = policy_for(&c, ws).await;
    run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    (Harness { core, c, task, ws, ctx }, ev)
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Runs `fut` (which must end the acceptance as not passed) and returns its panic message.
async fn not_passed_message(fut: impl std::future::Future<Output = ()> + Send + 'static) -> String {
    let err = tokio::spawn(fut).await.unwrap_err();
    let payload = err.into_panic();
    match payload.downcast::<String>() {
        Ok(m) => *m,
        Err(p) => p.downcast::<&str>().map(|m| (*m).to_owned()).unwrap_or_else(|_| "non-string panic".into()),
    }
}

#[tokio::test]
async fn an_invalid_severity_is_repaired_and_the_corrected_report_accepted() {
    let (h, ev) = harness(
        serde_json::json!([{ "report": review_report("minor") }, { "report": review_report("low") }]),
        |core| Evidence::new(core.dir.path()),
    )
    .await;
    let d = run_phase(&h.c, h.task, Role::Review, "claude_code", h.ws, &h.ctx, &ev).await;
    // The corrected report was accepted, with the original finding.
    let result = d.messages.iter().rev().find(|m| m.kind == MessageKind::ReviewResult).unwrap();
    assert_eq!(result.payload["report"]["findings"][0]["severity"], "low");
    assert_eq!(result.payload["report"]["findings"][0]["title"], "isdigit() accepts non-ASCII digits");
    // The nudge forwarded Core's exact error and the allowed values.
    let notes: Vec<&str> = d
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::OperatorNote)
        .filter_map(|m| m.payload["text"].as_str())
        .collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("unknown variant `minor`") && notes[0].contains(ALLOWED), "{}", notes[0]);
    // Evidence: the rejection and nudge, and every message in order.
    let repairs = lines(&h.core.dir.path().join("evidence/repairs.jsonl"));
    assert_eq!(repairs.len(), 1);
    assert_eq!(repairs[0]["nudge"], 1);
    assert!(repairs[0]["error"].as_str().unwrap().contains("unknown variant `minor`"));
    let kinds: Vec<String> = lines(&h.core.dir.path().join("evidence/messages.jsonl"))
        .iter()
        .filter(|m| m["phase"] == "Review by claude_code")
        .map(|m| m["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["task_assignment", "malformed_report", "operator_note", "review_result"], "{kinds:?}");
}

#[tokio::test]
async fn a_second_invalid_report_uses_the_second_nudge() {
    let steps = serde_json::json!([
        { "report": review_report("minor") },
        { "report": review_report("minor") },
        { "report": review_report("low") }
    ]);
    let (h, ev) = harness(steps, |core| Evidence::new(core.dir.path())).await;
    let d = run_phase(&h.c, h.task, Role::Review, "claude_code", h.ws, &h.ctx, &ev).await;
    assert!(d.messages.iter().any(|m| m.kind == MessageKind::ReviewResult));
    let repairs = lines(&h.core.dir.path().join("evidence/repairs.jsonl"));
    assert_eq!(
        repairs.iter().map(|r| r["nudge"].clone()).collect::<Vec<_>>(),
        [serde_json::json!(1), serde_json::json!(2)]
    );
    assert_eq!(d.messages.iter().filter(|m| m.kind == MessageKind::OperatorNote).count(), MAX_NUDGES);
}

#[tokio::test]
async fn exhausted_repairs_end_the_acceptance_as_not_passed() {
    let steps = serde_json::json!([
        { "report": review_report("minor") },
        { "report": review_report("minor") },
        { "report": review_report("minor") }
    ]);
    let (h, ev) = harness(steps, |core| Evidence::new(core.dir.path())).await;
    let dir = h.core.dir.path().to_path_buf();
    let Harness { core, c, task, ws, ctx } = h;
    let message = not_passed_message(async move {
        run_phase(&c, task, Role::Review, "claude_code", ws, &ctx, &ev).await;
    })
    .await;
    assert!(
        message.starts_with("ACCEPTANCE NOT PASSED: report repair exhausted in phase Review by claude_code"),
        "{message}"
    );
    let repairs = lines(&dir.join("evidence/repairs.jsonl"));
    assert_eq!(repairs.len(), 3);
    assert_eq!(repairs[2]["exhausted"], true);
    let t: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("evidence/termination.json")).unwrap())
            .unwrap();
    assert_eq!(
        (t["outcome"].as_str(), t["phase"].as_str()),
        (Some("NOT PASSED"), Some("Review by claude_code"))
    );
    assert!(dir.join("evidence/not-passed/core-records.json").exists());
    drop(core);
}

/// The complete evidence set a not-passed run must leave.
fn assert_complete_evidence(dir: &Path, snapshot: &str) {
    let root = dir.join("evidence");
    let records: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(snapshot).join("core-records.json")).unwrap(),
    )
    .unwrap();
    for table in ["events", "tasks", "task_assignments", "messages", "sessions", "approvals", "workspaces"] {
        assert!(records[table].is_array(), "{table}");
    }
    assert!(!records["events"].as_array().unwrap().is_empty());
    let ws: Vec<String> = std::fs::read_dir(root.join(snapshot).join("workspaces"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(["status", "diff", "head"].iter().all(|ext| ws.iter().any(|f| f.ends_with(ext))), "{ws:?}");
    for f in ["refs.txt", "log.txt", "status.txt", "worktrees.txt"] {
        assert!(root.join(snapshot).join("git").join(f).exists(), "{f}");
    }
    assert!(root.join(snapshot).join("transcripts/manifest.txt").exists());
    assert!(root.join("run.log").exists() && root.join("messages.jsonl").exists());
    assert!(root.join("OUTCOME.txt").exists());
}

#[tokio::test]
async fn deadline_expiry_preserves_the_complete_evidence_set() {
    // The review must ask for approval before it can report, so it is still
    // running when the (zero) deadline is checked.
    let steps =
        serde_json::json!({ "tool": "tool:Bash|{\"command\":\"ls\"}", "report": review_report("low") });
    let (h, ev) = harness(steps, |core| Evidence::new(core.dir.path())).await;
    let dir = h.core.dir.path().to_path_buf();
    let Harness { core, c, task, ws, ctx } = h;
    let message = not_passed_message(async move {
        ev.set_phase("Review by claude_code");
        assign(&c, task, Role::Review, "claude_code", ws).await;
        drive(&c, task, TaskStatus::InReview, &ctx, &ev, Duration::ZERO).await;
    })
    .await;
    assert!(
        message.starts_with(
            "ACCEPTANCE NOT PASSED: deadline: phase Review by claude_code was still in_review after 0 s"
        ),
        "{message}"
    );
    let t: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("evidence/termination.json")).unwrap())
            .unwrap();
    assert_eq!(t["phase"], "Review by claude_code");
    assert!(t["reason"].as_str().unwrap().starts_with("deadline:"));
    assert_eq!(t["phase_deadline_secs"], PHASE_DEADLINE.as_secs());
    assert_complete_evidence(&dir, "not-passed");
    drop(core);
}

#[tokio::test]
async fn an_unexpected_panic_preserves_the_evidence() {
    let (h, ev) =
        harness(serde_json::json!({ "report": review_report("low") }), |core| Evidence::new(core.dir.path()))
            .await;
    let dir = h.core.dir.path().to_path_buf();
    // The Core (and its directory) outlives the panicking work.
    let message = not_passed_message(async move {
        ev.set_phase("operator: validate");
        panic!("injected failure in the operator half");
    })
    .await;
    assert_eq!(message, "injected failure in the operator half");
    let t: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("evidence/termination.json")).unwrap())
            .unwrap();
    assert_eq!(
        (t["outcome"].as_str(), t["phase"].as_str()),
        (Some("NOT PASSED"), Some("operator: validate"))
    );
    assert!(
        t["reason"].as_str().unwrap().starts_with("unexpected panic: injected failure in the operator half"),
        "{t}"
    );
    assert_complete_evidence(&dir, "panic");
    drop(h);
}

// --- Free acceptance-harness gaps: session stop, correction cycle, report repair ----

/// Events from Core's journal matching `column = id`, in order: `(seq, type, payload)`.
fn journal(dir: &Path, column: &str, id: Uuid) -> Vec<(i64, String, serde_json::Value)> {
    let db = read_db(dir);
    let mut st = db
        .prepare(&format!("SELECT seq, event_type, payload FROM events WHERE {column} = ?1 ORDER BY seq"))
        .unwrap();
    st.query_map([id.to_string()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })
    .unwrap()
    .map(|r| {
        let (seq, t, p) = r.unwrap();
        (seq, t, serde_json::from_str(&p).unwrap())
    })
    .collect()
}

/// The prompts Core delivered to `session` (`agent.turn_started`), in order,
/// with whether the journal truncated them (it keeps the first 4096 bytes).
fn prompts(dir: &Path, session: Uuid) -> Vec<(String, bool)> {
    journal(dir, "session_id", session)
        .into_iter()
        .filter(|(_, t, _)| t == "agent.turn_started")
        .map(|(_, _, p)| (p["prompt"].as_str().unwrap().to_owned(), p["prompt_truncated"].as_bool().unwrap()))
        .collect()
}

/// The user messages the fake Codex process received in its thread, in full
/// (it persists them for resume in `$TMPDIR/switchyard-fake-codex/`).
fn fake_codex_received(native_thread: &str) -> Vec<String> {
    let file =
        std::env::temp_dir().join("switchyard-fake-codex").join(format!("thread-{native_thread}.json"));
    let thread: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    thread["history"].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_owned()).collect()
}

/// `(from, to, reason)` of every `task.status_changed` of `task`.
fn status_changes(dir: &Path, task: Uuid) -> Vec<(String, String, Option<String>)> {
    journal(dir, "task_id", task)
        .into_iter()
        .filter(|(_, t, _)| t == "task.status_changed")
        .map(|(_, _, p)| {
            (
                p["from"].as_str().unwrap().to_owned(),
                p["to"].as_str().unwrap().to_owned(),
                p["reason"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// `s` as it appears inside a JSON string.
fn json_escaped(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

/// A task with a fake scenario `spec` in a durations project.
async fn fake_task(c: &CoreClient, project: Uuid, title: &str, spec: &serde_json::Value) -> Uuid {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    c.call::<TasksCreate>(TasksCreateParams {
        project_id: project,
        title: title.into(),
        description: Some(format!("{DURATIONS_SPEC} [fake:report:{hex}]")),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec![],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    })
    .await
    .unwrap()
    .task
    .id
}

/// Experiment 1's failure, reproduced without paid agents: a review is
/// recorded while the reviewer's process is still exiting (`linger_ms`), so
/// the task reads as reviewed while a session is still attached. Core
/// refuses the operator's commit then, with exactly the error the real run
/// hit; after the harness waits for the session to be terminal the same
/// commit succeeds. Core's rule is unchanged; only the harness waits.
#[tokio::test]
async fn the_operator_commits_only_after_the_reviewers_session_has_stopped() {
    // Core's stop must not kill the slow exit early (tests default to 2 s).
    let core = TestCore::start_with(|cfg| cfg.agent_stop_grace = Duration::from_secs(20)).await;
    let ev = Evidence::new(core.dir.path());
    let (c, _rx) = core.connect().await;
    let (repo, project, _check, base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);
    let ws = new_workspace(&c, project).await;
    let approved = serde_json::json!({
        "type": "review", "summary": "Fine", "result": "approved", "findings": [],
        "acceptance": [{ "criterion": "valid durations return the right number of seconds", "met": true }]
    });
    let spec = serde_json::json!({
        "linger_ms": 8000,
        "implement": { "tool": "edit:durations.py", "report": { "type": "implementation", "summary": "Done." } },
        "review": { "report": approved },
    });
    let task = fake_task(&c, project, "Race", &spec).await;
    let ctx = policy_for(&c, ws).await;
    run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    // The review phase without the harness's wait, as experiment 1 had it.
    ev.set_phase("Review by claude_code");
    assign(&c, task, Role::Review, "claude_code", ws).await;
    let d = drive(&c, task, TaskStatus::InReview, &ctx, &ev, PHASE_DEADLINE).await;
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    assert_eq!(d.task.status_reason.as_deref(), Some("review: approved"));
    let reviewer = session_of_role(&d, Role::Review);
    let s = c.call::<SessionsGet>(SessionIdParams { session_id: reviewer }).await.unwrap().session;
    assert!(s.state.is_live(), "the reviewer is still exiting when the review is recorded: {:?}", s.state);
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: Some(task),
            workspace_id: None,
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    let refused = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Parse durations".into(),
        })
        .await
        .unwrap_err();
    let refused = format!("{refused:?}");
    ev.log(&format!("operator commit while the reviewer was still attached: {refused}"));
    assert!(
        refused.contains("InvalidState")
            && refused.contains("an agent session is running in the workspace; stop it first"),
        "Core refuses while the session is attached: {refused}"
    );
    // The harness's wait: an explicit state condition, bounded.
    ev.set_phase("operator: wait for the reviewer's session");
    let s = await_sessions_stopped(&c, ws, reviewer, &ev, SESSION_STOP_DEADLINE).await;
    assert!(s.state.is_terminal(), "{:?}", s.state);
    let stops = lines(&core.dir.path().join("evidence/session_stops.jsonl"));
    let last = stops.last().unwrap();
    assert_eq!(last["session"], reviewer.to_string());
    assert_eq!(last["final_state"], "stopped");
    assert_ne!(last["state_when_reported"], "stopped");
    // Now the same operator action succeeds.
    ev.set_phase("operator: commit");
    let i = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Parse durations".into(),
        })
        .await
        .unwrap()
        .integration;
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "the target is untouched");
    ev.log(&format!("committed after the wait: integration {} is {:?}", i.id, i.status));
    ev.passed();
}

/// A session that does not stop within the bound ends acceptance as not
/// passed, before any operator action, with the complete evidence set.
#[tokio::test]
async fn a_reviewer_session_that_does_not_stop_in_time_preserves_the_evidence() {
    let core = TestCore::start_with(|_| {}).await;
    let dir = core.dir.path().to_path_buf();
    let ev = Evidence::new(&dir);
    let (c, _rx) = core.connect().await;
    let (repo, project, _check, _base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);
    let ws = new_workspace(&c, project).await;
    let spec = serde_json::json!({
        "linger_ms": 5000,
        "implement": { "report": { "type": "implementation", "summary": "Done." } },
        "review": { "report": review_report("low") },
    });
    let task = fake_task(&c, project, "Slow stop", &spec).await;
    let ctx = policy_for(&c, ws).await;
    run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    let c2 = c.clone();
    let message = not_passed_message(async move {
        ev.set_phase("Review by claude_code");
        assign(&c2, task, Role::Review, "claude_code", ws).await;
        let d = drive(&c2, task, TaskStatus::InReview, &ctx, &ev, PHASE_DEADLINE).await;
        await_sessions_stopped(&c2, ws, session_of_role(&d, Role::Review), &ev, Duration::ZERO).await;
        unreachable!("the wait expired");
    })
    .await;
    assert!(
        message
            .starts_with("ACCEPTANCE NOT PASSED: session stop: in phase Review by claude_code the session "),
        "{message}"
    );
    assert!(message.contains("the next operator action was not attempted"), "{message}");
    let t: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("evidence/termination.json")).unwrap())
            .unwrap();
    assert_eq!(
        (t["outcome"].as_str(), t["phase"].as_str()),
        (Some("NOT PASSED"), Some("Review by claude_code"))
    );
    assert!(t["reason"].as_str().unwrap().starts_with("session stop:"), "{t}");
    assert_complete_evidence(&dir, "not-passed");
    // No operator action was attempted.
    let integrations: i64 =
        read_db(&dir).query_row("SELECT COUNT(*) FROM integrations", [], |r| r.get(0)).unwrap();
    assert_eq!(integrations, 0);
    drop(core);
}

/// One structured review finding.
fn finding(severity: &str, title: &str, line: u32, explanation: &str, suggestion: &str) -> serde_json::Value {
    serde_json::json!({
        "severity": severity, "title": title, "path": "durations.py", "line": line,
        "explanation": explanation, "suggestion": suggestion
    })
}

/// After a phase: the task, every assignment so far (oldest first), and their sessions.
fn check_phase(d: &TaskDetail, reason: &str, done: &[(Role, &str, &str)]) {
    assert_eq!(d.task.status, TaskStatus::AwaitingReview, "{:?}", d.task.status_reason);
    assert_eq!(d.task.status_reason.as_deref(), Some(reason));
    assert!(d.task.open_assignment.is_none());
    let mut a: Vec<_> = d.assignments.iter().collect();
    a.sort_by_key(|a| a.created_at_ms);
    let got: Vec<(Role, &str, AssignmentStatus, Option<&str>)> =
        a.iter().map(|a| (a.role, a.provider.as_str(), a.status, a.outcome.as_deref())).collect();
    let want: Vec<(Role, &str, AssignmentStatus, Option<&str>)> =
        done.iter().map(|(r, p, o)| (*r, *p, AssignmentStatus::Reported, Some(*o))).collect();
    assert_eq!(got, want);
    assert!(a.iter().all(|a| a.session_id.is_some() && a.ended_at_ms.is_some()));
}

async fn assert_terminal(c: &CoreClient, session: Uuid) {
    let s = c.call::<SessionsGet>(SessionIdParams { session_id: session }).await.unwrap().session;
    assert!(s.state.is_terminal(), "terminal before the next operator action: {:?}", s.state);
}

/// The whole correction cycle with fake agents (no paid sessions): Claude
/// investigates, Codex implements, the reviewer requests changes with
/// structured findings, Codex gets exactly those findings in its next
/// assignment's context and corrects, and the reviewer approves. Task and
/// assignment transitions are checked after every phase, and every
/// reported session is terminal before the next operator action.
#[tokio::test]
async fn fake_correction_cycle_carries_review_findings_to_the_next_implementation() {
    let core = TestCore::start_with(|_| {}).await;
    let dir = core.dir.path().to_path_buf();
    let ev = Evidence::named(&dir, "evidence-correction");
    let (c, _rx) = core.connect().await;
    let (repo, project, _check, base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);
    let ws = new_workspace(&c, project).await;
    let ws_path = PathBuf::from(
        c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: ws }).await.unwrap().workspace.path,
    );
    let investigation = serde_json::json!({
        "type": "findings",
        "summary": "parse_duration is not implemented",
        "findings": [finding("high", "Not implemented", 2, "parse_duration raises NotImplementedError", "implement it")]
    });
    let review_findings = serde_json::json!([
        finding("high", "Non-ASCII digits accepted", 13, "str.isdigit() accepts '١h'", "accept only 0-9"),
        finding("medium", "Leading zeros accepted", 18, "'01h' returns 3600", "use 0|[1-9][0-9]*"),
    ]);
    let changes = serde_json::json!({
        "type": "review", "summary": "Two problems", "result": "changes_requested",
        "findings": review_findings,
        "acceptance": [{ "criterion": "digits are ASCII only", "met": false }]
    });
    let approved = serde_json::json!({
        "type": "review", "summary": "Both findings are addressed", "result": "approved", "findings": [],
        "acceptance": [{ "criterion": "digits are ASCII only", "met": true }]
    });
    let spec = serde_json::json!({
        // Every Claude process exits slowly, so each wait is really exercised.
        "linger_ms": 1500,
        "investigate": { "report": investigation },
        "implement": [
            { "tool": "edit:durations.py", "report": { "type": "implementation", "summary": "Implemented." } },
            { "tool": "edit:durations.py", "report": { "type": "implementation", "summary": "Addressed both findings." } }
        ],
        "review": [{ "report": changes }, { "report": approved }],
    });
    let task = fake_task(&c, project, "Correction cycle", &spec).await;
    let ctx = policy_for(&c, ws).await;
    let lines_of = || std::fs::read_to_string(ws_path.join("durations.py")).unwrap().lines().count();
    let base_lines = lines_of();

    let inv = (Role::Investigate, "claude_code", "reported: findings");
    let imp = (Role::Implement, "codex", "reported: implementation");
    let rev_changes = (Role::Review, "claude_code", "review: changes_requested");
    let rev_ok = (Role::Review, "claude_code", "review: approved");

    // 1. Claude investigates.
    let d = run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;
    check_phase(&d, "investigate reported: findings", &[inv]);
    assert_terminal(&c, session_of_role(&d, Role::Investigate)).await;
    assert_eq!(d.findings.len(), 1);

    // 2. Codex implements.
    let d = run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    check_phase(&d, "implement reported: implementation", &[inv, imp]);
    let first_impl = session_of_role(&d, Role::Implement);
    assert_terminal(&c, first_impl).await;
    assert_eq!(lines_of(), base_lines + 1, "the first implementation changed durations.py");

    // 3. The reviewer requests changes with structured findings.
    let d = run_phase(&c, task, Role::Review, "claude_code", ws, &ctx, &ev).await;
    check_phase(&d, "review: changes_requested", &[inv, imp, rev_changes]);
    let first_review = session_of_role(&d, Role::Review);
    assert_terminal(&c, first_review).await;
    let result = d.messages.iter().rev().find(|m| m.kind == MessageKind::ReviewResult).unwrap();
    assert_eq!(result.payload["effective_result"], "changes_requested");
    assert_eq!(result.payload["report"]["findings"], review_findings);

    // 4. Codex corrects, given exactly those findings.
    let d = run_phase(&c, task, Role::Implement, "codex", ws, &ctx, &ev).await;
    check_phase(&d, "implement reported: implementation", &[inv, imp, rev_changes, imp]);
    let second_impl = session_of_role(&d, Role::Implement);
    // A new assignment; Core resumed the implementer's earlier session for it.
    assert_terminal(&c, second_impl).await;
    assert_eq!(lines_of(), base_lines + 2, "the correction changed durations.py again");
    let mut implements: Vec<_> = d.assignments.iter().filter(|a| a.role == Role::Implement).collect();
    implements.sort_by_key(|a| a.created_at_ms);
    let ctx_findings = |a: &switchyard_protocol::model::TaskAssignment| {
        a.context.as_ref().unwrap()["review_findings"].clone()
    };
    // Core gives an implementer the task's open findings: the first one got
    // the investigation's; the correction gets exactly the review's (the
    // investigation's were no longer open after the first implementation).
    assert_eq!(ctx_findings(implements[0]), investigation["findings"]);
    assert_eq!(ctx_findings(implements[1]), review_findings);
    let open: Vec<(String, String)> =
        d.findings.iter().map(|f| (f.title.clone(), f.status.clone())).collect();
    ev.log(&format!("task findings and their status after the correction: {open:?}"));
    // Codex received them. Core journaled the delivery of the correction's
    // context to the implementer's session (`agent.turn_started`, a 4096-byte
    // prefix), and the Codex process itself received the whole context with
    // exactly those findings (the fake persists every user message).
    let delivered = prompts(&dir, second_impl);
    let (journaled, _) = delivered.last().unwrap();
    assert!(journaled.contains(&implements[1].id.to_string()), "{journaled}");
    let native = c
        .call::<SessionsGet>(SessionIdParams { session_id: second_impl })
        .await
        .unwrap()
        .session
        .native_session_id
        .unwrap();
    let received = fake_codex_received(&native);
    let correction_prompt = received.last().unwrap();
    assert!(correction_prompt.starts_with(journaled.as_str()), "Codex received what Core journaled");
    // The context is the first JSON object in the prompt (instructions follow it).
    let sent: serde_json::Value =
        serde_json::Deserializer::from_str(&correction_prompt[correction_prompt.find('{').unwrap()..])
            .into_iter()
            .next()
            .unwrap()
            .unwrap();
    assert_eq!(sent["assignment_id"], implements[1].id.to_string());
    assert_eq!(
        sent["review_findings"], review_findings,
        "Codex's context carries exactly the review findings"
    );
    assert_eq!(&sent, implements[1].context.as_ref().unwrap(), "the context Core recorded is what Codex got");
    ev.record(
        "correction_context",
        serde_json::json!({ "assignment": implements[1].id, "session": second_impl, "native_thread": native,
                            "received_by_codex": correction_prompt }),
    );
    let started: Vec<(String, String, bool)> = journal(&dir, "task_id", task)
        .into_iter()
        .filter(|(_, t, _)| t == "task.assignment_started")
        .map(|(_, _, p)| {
            (
                p["assignment_id"].as_str().unwrap().to_owned(),
                p["session_id"].as_str().unwrap().to_owned(),
                p["resumed"].as_bool().unwrap(),
            )
        })
        .collect();
    let second_start = started.iter().find(|(a, _, _)| *a == implements[1].id.to_string()).unwrap();
    assert_eq!(second_start.1, second_impl.to_string());
    ev.log(&format!(
        "correction assignment {} ran in session {} (resumed: {}; first implementation session {first_impl})",
        implements[1].id, second_impl, second_start.2
    ));
    // Both implementation patches (one per assignment) were approved by the
    // operator policy, in the workspace.
    let mut sessions = vec![first_impl, second_impl];
    sessions.dedup();
    let mut approvals = Vec::new();
    for s in sessions {
        approvals.extend(
            c.call::<ApprovalsList>(ApprovalsListParams { session_id: Some(s), ..Default::default() })
                .await
                .unwrap()
                .approvals,
        );
    }
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    for a in &approvals {
        assert_eq!(a.status, ApprovalStatus::Approved);
        assert_eq!(a.policy.as_ref().map(|p| p.category), Some(OperationCategory::FileEdit));
    }

    // 5. The reviewer approves.
    let d = run_phase(&c, task, Role::Review, "claude_code", ws, &ctx, &ev).await;
    check_phase(&d, "review: approved", &[inv, imp, rev_changes, imp, rev_ok]);
    let second_review = session_of_role(&d, Role::Review);
    ev.log(&format!("reviews ran in sessions {first_review} and {second_review}"));
    assert_terminal(&c, second_review).await;

    // The full task transition history, from Core's journal.
    let mut want = vec![("draft".to_owned(), "ready".to_owned(), None)];
    for (role, provider, working, reported) in [
        ("investigate", "claude_code", "in_progress", "investigate reported: findings"),
        ("implement", "codex", "in_progress", "implement reported: implementation"),
        ("review", "claude_code", "in_review", "review: changes_requested"),
        ("implement", "codex", "in_progress", "implement reported: implementation"),
        ("review", "claude_code", "in_review", "review: approved"),
    ] {
        let from = if want.len() == 1 { "ready" } else { "awaiting_review" };
        want.push((from.to_owned(), "assigned".to_owned(), Some(format!("{role} assigned to {provider}"))));
        want.push(("assigned".to_owned(), working.to_owned(), Some(format!("{role} session started"))));
        want.push((working.to_owned(), "awaiting_review".to_owned(), Some(reported.to_owned())));
    }
    assert_eq!(status_changes(&dir, task), want);
    // Each assignment: assigned → started → ended, in order, never overlapping.
    let lifecycle: Vec<(String, String, serde_json::Value)> = journal(&dir, "task_id", task)
        .into_iter()
        .filter(|(_, t, _)| t.starts_with("task.assign"))
        .map(|(_, t, p)| (t, p["assignment_id"].as_str().unwrap().to_owned(), p["status"].clone()))
        .collect();
    assert_eq!(lifecycle.len(), 15, "{lifecycle:?}");
    for chunk in lifecycle.chunks(3) {
        assert_eq!(
            chunk.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(),
            ["task.assigned", "task.assignment_started", "task.assignment_ended"]
        );
        assert!(chunk.iter().all(|(_, a, _)| *a == chunk[0].1));
        assert_eq!(chunk[2].2, "reported");
    }
    // Every reported session was terminal before the next step, and at least
    // one slow Claude exit really was waited for.
    let stops = lines(&dir.join("evidence-correction/session_stops.jsonl"));
    assert_eq!(stops.len(), 5, "{stops:?}");
    assert!(stops.iter().all(|s| s["final_state"] == "stopped"), "{stops:?}");
    assert!(
        stops.iter().any(|s| s["phase"].as_str().unwrap().ends_with("claude_code")
            && s["state_when_reported"] != "stopped"),
        "a lingering Claude session was still live when its report was seen: {stops:?}"
    );

    // The next operator action after the approval succeeds at once.
    ev.set_phase("operator: commit");
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: Some(task),
            workspace_id: None,
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    c.call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
        integration_id: i.id,
        message: "Parse durations".into(),
    })
    .await
    .unwrap();
    assert_eq!(git(&repo, &["rev-parse", "main"]), base);
    ev.passed();
}

/// The parser error Core records for a findings report with severity `minor`.
const MINOR_SEVERITY_ERROR: &str = "the report is not valid: unknown variant `minor`, expected one of `info`, \
                                    `low`, `medium`, `high`, `critical`";

/// Report repair with a fake agent (no paid sessions): an investigation
/// report with the invalid severity `minor` is rejected by Core, the exact
/// parser error reaches the same agent session, and the corrected report is
/// accepted. Both reports and the repair message stay in the journal and the
/// evidence.
#[tokio::test]
async fn fake_report_repair_reaches_the_same_session_and_is_accepted() {
    let core = TestCore::start_with(|_| {}).await;
    let dir = core.dir.path().to_path_buf();
    let ev = Evidence::named(&dir, "evidence-repair");
    let (c, _rx) = core.connect().await;
    let (repo, project, _check, _base) = durations_project(&core, &c).await;
    ev.set_repo(&repo);
    let ws = new_workspace(&c, project).await;
    let findings = |severity: &str| {
        serde_json::json!({
            "type": "findings",
            "summary": "isdigit() accepts non-ASCII digits",
            "findings": [finding(severity, "Non-ASCII digits accepted", 13, "'١h' parses as 3600", "accept only 0-9")]
        })
    };
    let spec = serde_json::json!({
        "investigate": [{ "report": findings("minor") }, { "report": findings("low") }]
    });
    let task = fake_task(&c, project, "Report repair", &spec).await;
    let ctx = policy_for(&c, ws).await;
    let d = run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;

    // Core rejected the first report with the parser's exact error.
    let rejected: Vec<_> = d.messages.iter().filter(|m| m.kind == MessageKind::MalformedReport).collect();
    assert_eq!(rejected.len(), 1);
    let error = rejected[0].payload["error"].as_str().unwrap();
    assert_eq!(error, MINOR_SEVERITY_ERROR);
    assert!(
        rejected[0].payload["excerpt"].as_str().unwrap().contains("\"minor\""),
        "{}",
        rejected[0].payload
    );
    // One assignment and one session for the whole repair.
    assert_eq!(d.assignments.len(), 1);
    let a = &d.assignments[0];
    assert_eq!((a.status, a.outcome.as_deref()), (AssignmentStatus::Reported, Some("reported: findings")));
    let session = a.session_id.unwrap();
    // The repair note carries the exact error and was delivered to that session.
    let notes: Vec<_> = d.messages.iter().filter(|m| m.kind == MessageKind::OperatorNote).collect();
    assert_eq!(notes.len(), 1);
    let note = notes[0].payload["text"].as_str().unwrap();
    assert!(note.contains(error), "{note}");
    assert_eq!(notes[0].delivery, "delivered");
    let delivered = prompts(&dir, session);
    assert_eq!(delivered.len(), 2, "the task context, then the repair: {delivered:?}");
    let (repair_prompt, truncated) = &delivered[1];
    assert!(!truncated, "the repair prompt is journaled in full");
    assert!(repair_prompt.contains(error), "the exact parser error reached the agent: {repair_prompt}");
    assert!(repair_prompt.contains(note), "{repair_prompt}");
    // The corrected report, from the same session, was accepted.
    let accepted = d.messages.iter().rev().find(|m| m.kind == MessageKind::Findings).unwrap();
    assert_eq!(accepted.sender_session_id, Some(session));
    assert_eq!(accepted.payload["report"]["findings"][0]["severity"], "low");
    assert_eq!(d.findings.len(), 1);
    assert_eq!(d.task.status_reason.as_deref(), Some("investigate reported: findings"));
    let kinds: Vec<MessageKind> = d.messages.iter().map(|m| m.kind).collect();
    assert_eq!(
        kinds,
        [
            MessageKind::TaskAssignment,
            MessageKind::MalformedReport,
            MessageKind::OperatorNote,
            MessageKind::Findings
        ]
    );
    // Both reports are preserved verbatim in the journal, from the same session.
    let reports: Vec<String> = journal(&dir, "session_id", session)
        .into_iter()
        .filter(|(_, t, _)| t == "agent.message")
        .map(|(_, _, p)| p["text"].as_str().unwrap().to_owned())
        .filter(|t| t.contains("```switchyard-report"))
        .collect();
    assert_eq!(reports.len(), 2, "{reports:?}");
    assert!(reports[0].contains("\"severity\":\"minor\""), "{}", reports[0]);
    assert!(reports[1].contains("\"severity\":\"low\""), "{}", reports[1]);
    // ... and in the evidence: the rejection, the repair message and both reports.
    ev.passed();
    let repairs = lines(&dir.join("evidence-repair/repairs.jsonl"));
    assert_eq!(repairs.len(), 1);
    assert_eq!(repairs[0]["error"], error);
    assert_eq!(repairs[0]["text"], note);
    let messages = lines(&dir.join("evidence-repair/messages.jsonl"));
    assert_eq!(
        messages.iter().map(|m| m["kind"].as_str().unwrap()).collect::<Vec<_>>(),
        ["task_assignment", "malformed_report", "operator_note", "findings"]
    );
    let records: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("evidence-repair/final/core-records.json")).unwrap(),
    )
    .unwrap();
    let snapshot = records["events"].to_string();
    for r in &reports {
        assert!(snapshot.contains(&json_escaped(r)), "report preserved in the snapshot: {r}");
    }
}

// --- Replay of the failed run's defective implementation ----------------------------

/// The preserved failed run (read only: never written by these tests).
const ORIGINAL_RUN: &str = "/var/folders/20/y08982ms65df0c_kwh11p_m80000gn/T/syfnrQ7O";
const ORIGINAL_IMPL: &str =
    "data/workspaces/01a1da28-127f-72b3-8fd1-c46d62edb860/01a1da28-12b0-70b4-8852-36372cc1c103/durations.py";
const ORIGINAL_IMPL_SHA1: &str = "510965ca89cd25e80a25987bd3fff9a17c2a1b7f";
const ORIGINAL_BASE: &str = "b367380dffffd7142d2528a441c78e203cbdd6da";
/// Non-ASCII digit inputs the original implementation accepts.
const UNICODE_CASES: [&str; 5] = ["١h", "1٥m", "٣٠m", "１h", "²s"];

/// The original acceptance check plus the five non-ASCII digit cases.
fn replay_check() -> String {
    let cases = UNICODE_CASES.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
    format!(
        "{}\nfor bad in [{cases}]:\n    try:\n        p(bad)\n    except ValueError:\n        continue\n    \
         raise AssertionError('accepted ' + repr(bad))\nprint('rejected all 5 non-ASCII digit inputs')",
        DURATIONS_CHECK.trim_end()
    )
}

fn sha1(path: &Path) -> String {
    let out = Command::new("shasum").args(["-a", "1"]).arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_owned()
}

/// A replay of the original project: a copy (clone, which only reads the
/// original) of the original repository at its base commit, the replay
/// validation command, and a workspace holding the original defective
/// implementation, uncommitted as it was. Returns (repo, project, check, base, ws).
async fn replay_project(
    core: &TestCore,
    c: &CoreClient,
    ev: &Evidence,
) -> (PathBuf, Uuid, Uuid, String, Uuid) {
    let (setup, pre) = seed_replay(core.dir.path(), c, ev, "replay", replay_check()).await;
    assert!(pre.contains("accepted '١h'"), "{pre}");
    setup
}

/// [`replay_project`] with a clone directory name and a validation script:
/// returns the setup and the operator-side check's output on the original
/// implementation (which must fail).
async fn seed_replay(
    dir: &Path,
    c: &CoreClient,
    ev: &Evidence,
    clone: &str,
    check_script: String,
) -> ((PathBuf, Uuid, Uuid, String, Uuid), String) {
    let original = Path::new(ORIGINAL_RUN);
    let impl_path = original.join(ORIGINAL_IMPL);
    assert_eq!(sha1(&impl_path), ORIGINAL_IMPL_SHA1, "the preserved implementation is unchanged");
    let bytes = std::fs::read(&impl_path).unwrap();
    let repo = dir.join(clone);
    let st = Command::new("git")
        .args(["clone", "-q", "--no-local", "--no-hardlinks", "--branch", "main"])
        .arg(original.join("acceptance"))
        .arg(&repo)
        .status()
        .unwrap();
    assert!(st.success());
    git(&repo, &["remote", "remove", "origin"]);
    assert_eq!(git(&repo, &["rev-parse", "main"]), ORIGINAL_BASE, "the original base commit");
    git(&repo, &["config", "user.name", "Switchyard Acceptance"]);
    git(&repo, &["config", "user.email", "acceptance@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    ev.set_repo(&repo);
    let project = c
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project
        .id;
    let check = c
        .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: project,
            command_id: None,
            name: "duration check".into(),
            argv: vec!["/usr/bin/python3".into(), "-B".into(), "-c".into(), check_script.clone()],
            timeout_secs: 60,
            enabled: true,
            position: None,
        })
        .await
        .unwrap()
        .commands[0]
        .id;
    let ws = new_workspace(c, project).await;
    let ws_path = PathBuf::from(
        c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: ws }).await.unwrap().workspace.path,
    );
    std::fs::write(ws_path.join("durations.py"), &bytes).unwrap();
    assert_eq!(
        sha1(&ws_path.join("durations.py")),
        ORIGINAL_IMPL_SHA1,
        "seeded with the exact original bytes"
    );
    // Evidence of the starting point, and that the check catches it
    // (run by the operator side on a copy, never in the workspace).
    let out = ev.root().join("replay");
    std::fs::create_dir_all(&out).unwrap();
    // A copy of the original implementation next to the check (kept as evidence).
    std::fs::write(out.join("durations.py"), &bytes).unwrap();
    std::fs::write(out.join("check.py"), &check_script).unwrap();
    let pre = Command::new("/usr/bin/python3").args(["-B", "check.py"]).current_dir(&out).output().unwrap();
    let pre_text = format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        pre.status.code(),
        String::from_utf8_lossy(&pre.stdout),
        String::from_utf8_lossy(&pre.stderr)
    );
    std::fs::write(out.join("check-against-initial.txt"), &pre_text).unwrap();
    ev.log(&format!("replay check against the original implementation: {pre_text}"));
    assert!(!pre.status.success(), "the check must fail on the original implementation: {pre_text}");
    ((repo, project, check, ORIGINAL_BASE.to_owned(), ws), pre_text)
}

/// The original task, with the non-ASCII digit cases added to its criteria.
async fn replay_task(c: &CoreClient, project: Uuid) -> Uuid {
    c.call::<TasksCreate>(TasksCreateParams {
        project_id: project,
        title: "Parse durations like 1h30m".into(),
        description: Some(DURATIONS_SPEC.into()),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec![
            "valid durations return the right number of seconds".into(),
            "every invalid form listed in the description raises ValueError".into(),
            format!("digits are ASCII only: {} each raise ValueError", UNICODE_CASES.join(", ")),
            "only durations.py changes".into(),
        ],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    })
    .await
    .unwrap()
    .task
    .id
}

/// Integrates nothing: commits the workspace and validates it through Core
/// with the operator's authorization of the exact command line shown.
async fn replay_validate(
    c: &CoreClient,
    ws: Uuid,
    check: Uuid,
    task: Option<Uuid>,
    ev: &Evidence,
) -> IntegrationDetail {
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: task,
            workspace_id: if task.is_some() { None } else { Some(ws) },
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    let i = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Parse durations".into(),
        })
        .await
        .unwrap()
        .integration;
    let patch = c
        .call::<IntegrationsPatch>(IntegrationsPatchParams {
            integration_id: i.id,
            path: None,
            uncommitted: false,
        })
        .await
        .unwrap()
        .patch;
    ev.log(&format!("workspace changes committed by the operator:\n{patch}"));
    let plan = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().plan;
    let shown = plan[0].command_line.clone();
    c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: vec![ValidationAuthorization { command_id: check, command_line: shown }],
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let d = loop {
        let d = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
        if d.integration.status != IntegrationStatus::Validating
            && d.runs.first().is_some_and(|r| r.status != RunStatus::Running)
        {
            break d;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let step = &d.runs[0].steps[0];
    ev.log(&format!(
        "validation run {} {:?}: exit {:?}, authorized by {:?}, decision {}\nstdout: {:?}\nstderr: {:?}",
        d.runs[0].id,
        d.runs[0].status,
        step.exit_code,
        step.authorized_by,
        step.policy_decision,
        step.stdout,
        step.stderr
    ));
    d
}

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn replay_recovers_the_failed_runs_defective_implementation() {
    let Some(core) = TestCore::start_real(RealCli { claude: true, codex: true }, |cfg| {
        cfg.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
    else {
        return;
    };
    let mut core = core;
    core.dir.disable_cleanup(true);
    let ev = Evidence::new(core.dir.path());
    ev.log(&format!("evidence directory (kept): {}", ev.root().display()));
    let (c, _rx) = core.connect().await;
    let (repo, project, check, base, ws) = replay_project(&core, &c, &ev).await;
    let task = replay_task(&c, project).await;
    let ctx = policy_for(&c, ws).await;

    // Session 1: Claude investigates the workspace as it is (the original implementation).
    let d = run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;
    let investigation = d.messages.iter().rev().find(|m| m.sender == "agent").map(|m| m.payload.clone());
    ev.log(&format!("investigation report: {investigation:?}"));
    // Sessions 2-5: Codex implements, Claude reviews; one correction round at most.
    let review = implement_and_review(&c, task, ws, true, &ctx, &ev).await;
    ev.log(&format!("review verdicts: {:?}", review.rounds));
    if review.rounds.last().unwrap().as_deref() != Some("review: approved") {
        ev.not_passed(&format!("the final review did not approve: {:?}", review.rounds));
    }
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "agents never touched the target");
    // `run_phase` waited for Core to stop every reported session.
    spend(&c, project).await;
    ev.write("agents");

    // The operator validates through Core: the replay check must pass.
    ev.set_phase("operator: validate through Core");
    let d = replay_validate(&c, ws, check, Some(task), &ev).await;
    let step = &d.runs[0].steps[0];
    if d.runs[0].status != RunStatus::Passed
        || !step.stdout.as_deref().is_some_and(|o| o.contains("rejected all 5 non-ASCII digit inputs"))
    {
        ev.not_passed(&format!("validation did not pass: {:?} {:?}", d.runs[0].status, step.stderr));
    }
    ev.passed();
}

/// The replay's setup without agents: the original bytes are seeded, and the
/// replay check, run through Core with the operator's authorization, fails
/// on them for the non-ASCII digits (so it really executes those cases).
#[tokio::test]
async fn replay_setup_reproduces_the_defect_without_agents() {
    if !Path::new(ORIGINAL_RUN).join(ORIGINAL_IMPL).exists() {
        eprintln!("skipped: the preserved failed run is not on this machine");
        return;
    }
    let core = TestCore::start_with(|_| {}).await;
    let ev = Evidence::new(core.dir.path());
    let (c, _rx) = core.connect().await;
    let (repo, project, check, base, ws) = replay_project(&core, &c, &ev).await;
    c.call::<IntegrationsSetSettings>(IntegrationsSetSettingsParams {
        project_id: project,
        require_review: false,
    })
    .await
    .unwrap();
    let d = replay_validate(&c, ws, check, None, &ev).await;
    let step = &d.runs[0].steps[0];
    assert_eq!(d.runs[0].status, RunStatus::Failed);
    assert!(
        step.stderr.as_deref().is_some_and(|e| e.contains("AssertionError: accepted '١h'")),
        "{:?}",
        step.stderr
    );
    assert!(
        step.stdout.as_deref().is_some_and(|o| o.contains("validated parse_duration")),
        "the original cases passed first"
    );
    assert_eq!(git(&repo, &["rev-parse", "main"]), base);
    assert_eq!(
        sha1(&Path::new(ORIGINAL_RUN).join(ORIGINAL_IMPL)),
        ORIGINAL_IMPL_SHA1,
        "the original is untouched"
    );
}

// --- Correction cycle (experiment 1) and report repair (experiment 2) --------------

/// Experiment 1: the original specification with four objective additions.
const EXP1_SPEC: &str = "Implement parse_duration(text) in durations.py. It returns the number of seconds in a \
     duration written as one or more <integer><unit> parts with the units d, h, m and s (a day is 86400 seconds), \
     each unit at most once and in that order: '1h30m' -> 5400, '45s' -> 45, '2m5s' -> 125, '1h0m7s' -> 3607, \
     '2d3h' -> 183600. Numbers use only the ASCII digits 0-9 and have no leading zeros: '0s' -> 0, but '01h' and \
     '00m' are invalid. Whitespace around the whole text is ignored. It raises ValueError for an empty or blank \
     string, an unknown unit, a part without a number, a repeated or out-of-order unit, a number with a leading \
     zero, a non-ASCII digit, and any other text. Change only durations.py and keep it small. Do not run Python.";

/// Experiment 2: the disclosed project convention that conflicts with
/// Switchyard's report schema (a deliberate, approved fixture).
const EXP2_CONVENTION: &str =
    " Classify every finding's severity on this project's scale: minor, major or blocker.";

fn exp1_check() -> String {
    let unicode = UNICODE_CASES.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
    format!(
        "from durations import parse_duration as p\n\
valid = {{'1h30m': 5400, '45s': 45, '2m5s': 125, '1h0m7s': 3607, ' 3m ': 180, '2d3h': 183600, '1d': 86400, \
'1d0h0m1s': 86401, '0s': 0, '10h': 36000}}\n\
for text, want in valid.items():\n    got = p(text)\n    assert got == want, (text, got, want)\n\
invalid = ['', '   ', '5', 'm', '5x', '30m1h', '1h1h', '1h 30m', '-5s', 'h5', '1h2d', '1d1d', '01h', '00m', \
'1h05m', {unicode}]\n\
for bad in invalid:\n    try:\n        p(bad)\n    except ValueError:\n        continue\n    \
raise AssertionError('accepted ' + repr(bad))\n\
print('all', len(valid), 'valid and', len(invalid), 'invalid inputs behave as required')"
    )
}

/// A correct experiment 1 implementation. Used only by the free setup test to
/// prove the check itself; never given to an agent.
const EXP1_REFERENCE: &str = concat!(
    "_UNITS = ['d', 'h', 'm', 's']\n",
    "_SECONDS = {'d': 86400, 'h': 3600, 'm': 60, 's': 1}\n",
    "\n",
    "\n",
    "def parse_duration(text):\n",
    "    text = text.strip()\n",
    "    if not text:\n",
    "        raise ValueError('empty duration')\n",
    "    total, pos, previous = 0, 0, -1\n",
    "    while pos < len(text):\n",
    "        start = pos\n",
    "        while pos < len(text) and text[pos] in '0123456789':\n",
    "            pos += 1\n",
    "        number = text[start:pos]\n",
    "        if not number or pos == len(text) or (len(number) > 1 and number[0] == '0'):\n",
    "            raise ValueError('invalid duration part')\n",
    "        unit = text[pos]\n",
    "        if unit not in _SECONDS or _UNITS.index(unit) <= previous:\n",
    "            raise ValueError('invalid unit')\n",
    "        previous = _UNITS.index(unit)\n",
    "        total += int(number) * _SECONDS[unit]\n",
    "        pos += 1\n",
    "    return total\n",
);

async fn exp1_task(c: &CoreClient, project: Uuid) -> Uuid {
    c.call::<TasksCreate>(TasksCreateParams {
        project_id: project,
        title: "Parse durations like 2d3h".into(),
        description: Some(EXP1_SPEC.into()),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec![
            "valid durations return the right number of seconds".into(),
            "every invalid form listed in the description raises ValueError".into(),
            format!("digits are ASCII only: {} each raise ValueError", UNICODE_CASES.join(", ")),
            "a d unit (86400 s) comes before h, in the order d, h, m, s: '2d3h' -> 183600".into(),
            "no leading zeros except a lone 0: '01h' raises ValueError, '0s' -> 0".into(),
            "only durations.py changes".into(),
        ],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    })
    .await
    .unwrap()
    .task
    .id
}

async fn exp2_task(c: &CoreClient, project: Uuid) -> Uuid {
    c.call::<TasksCreate>(TasksCreateParams {
        project_id: project,
        title: "Parse durations like 1h30m".into(),
        description: Some(format!("{DURATIONS_SPEC}{EXP2_CONVENTION}")),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec![
            "valid durations return the right number of seconds".into(),
            "every invalid form listed in the description raises ValueError".into(),
            format!("digits are ASCII only: {} each raise ValueError", UNICODE_CASES.join(", ")),
            "only durations.py changes".into(),
        ],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    })
    .await
    .unwrap()
    .task
    .id
}

fn read_db(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        dir.join("data/switchyard.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

/// Experiment 1: investigate → implement → review → (correct → review), then
/// Core validates whatever the agents ended with.
async fn experiment1(c: CoreClient, dir: PathBuf) -> serde_json::Value {
    let ev = Evidence::named(&dir, "evidence-exp1");
    ev.log("experiment 1: correction cycle");
    let ((repo, project, check, base, ws), _pre) =
        seed_replay(&dir, &c, &ev, "replay-exp1", exp1_check()).await;
    let task = exp1_task(&c, project).await;
    let ctx = policy_for(&c, ws).await;
    run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;
    let review = implement_and_review(&c, task, ws, true, &ctx, &ev).await;
    let approved = review.rounds.last().unwrap().as_deref() == Some("review: approved");
    let correction_cycle =
        review.rounds.len() == 2 && review.rounds[0].as_deref() != Some("review: approved");
    // What each review found, and what the second implementation was given.
    let d = c.call::<TasksGet>(TaskIdParams { task_id: task }).await.unwrap();
    let reviews: Vec<serde_json::Value> = d
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::ReviewResult)
        .map(|m| m.payload.clone())
        .collect();
    let contexts: Vec<serde_json::Value> = {
        let db = read_db(&dir);
        let mut st = db
            .prepare("SELECT context FROM task_assignments WHERE task_id = ?1 AND role = 'implement' ORDER BY created_at")
            .unwrap();
        st.query_map([task.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| serde_json::from_str(&c.unwrap()).unwrap())
            .collect()
    };
    let correction = serde_json::json!({
        "review_verdicts": review.rounds,
        "correction_cycle": correction_cycle,
        "reviews": reviews,
        "implement_contexts_review_findings": contexts.iter().map(|c| c["review_findings"].clone()).collect::<Vec<_>>(),
    });
    let _ =
        std::fs::write(ev.root().join("correction.json"), serde_json::to_string_pretty(&correction).unwrap());
    ev.log(&format!("review verdicts {:?}; genuine correction cycle: {correction_cycle}", review.rounds));
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "agents never touched the target");
    ev.write("agents");

    // Core validates independently, whatever the reviews said. The last
    // reviewer's session must have ended first (Core refuses integration
    // steps while a session is attached); `run_phase` waited for it, and this
    // re-checks it right before the operator acts.
    ev.set_phase("operator: validate through Core");
    let reviewer = session_of_role(&d, Role::Review);
    await_sessions_stopped(&c, ws, reviewer, &ev, SESSION_STOP_DEADLINE).await;
    let v = replay_validate(&c, ws, check, Some(task), &ev).await;
    let step = &v.runs[0].steps[0];
    let validated = v.runs[0].status == RunStatus::Passed
        && step.stdout.as_deref().is_some_and(|o| o.contains("invalid inputs behave as required"));
    let summary = serde_json::json!({
        "task": task,
        "review_verdicts": review.rounds,
        "correction_cycle": correction_cycle,
        "final_review_approved": approved,
        "validation": { "run": v.runs[0].id, "status": v.runs[0].status, "exit_code": step.exit_code,
                        "stdout": step.stdout, "stderr": step.stderr },
    });
    let _ = std::fs::write(ev.root().join("summary.json"), serde_json::to_string_pretty(&summary).unwrap());
    if !approved {
        ev.not_passed(&format!(
            "the final review did not approve: {:?}; validation {:?}",
            review.rounds, v.runs[0].status
        ));
    }
    if !validated {
        ev.not_passed(&format!("the approved implementation failed Core's validation: {:?}", step.stderr));
    }
    ev.passed();
    summary
}

/// Experiment 2: one investigation under the disclosed severity convention.
async fn experiment2(c: CoreClient, dir: PathBuf) -> serde_json::Value {
    let ev = Evidence::named(&dir, "evidence-exp2");
    ev.log("experiment 2: report repair under a conflicting project convention");
    let ((_repo, project, _check, _base, ws), _pre) =
        seed_replay(&dir, &c, &ev, "replay-exp2", replay_check()).await;
    let task = exp2_task(&c, project).await;
    let ctx = policy_for(&c, ws).await;
    let d = run_phase(&c, task, Role::Investigate, "claude_code", ws, &ctx, &ev).await;
    let session = d.assignments.iter().find(|a| a.role == Role::Investigate).and_then(|a| a.session_id);
    // Every message the agent sent, in full, from the journal; and which ones Core rejected.
    let db = read_db(&dir);
    let agent_messages: Vec<(i64, i64, String)> = {
        let mut st = db
            .prepare(
                "SELECT seq, occurred_at, json_extract(payload, '$.text') FROM events \
                 WHERE event_type = 'agent.message' AND session_id = ?1 ORDER BY seq",
            )
            .unwrap();
        st.query_map([session.map(|s| s.to_string()).unwrap_or_default()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let rejected: Vec<serde_json::Value> = d
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::MalformedReport)
        .map(|m| {
            let at = m.created_at_ms;
            let original = agent_messages.iter().rev().find(|(_, t, _)| *t <= at).map(|(seq, _, text)| (seq, text));
            serde_json::json!({
                "message": m.id, "error": m.payload["error"], "excerpt": m.payload["excerpt"],
                "original_report_event_seq": original.map(|o| o.0), "original_report_text": original.map(|o| o.1),
            })
        })
        .collect();
    let notes: Vec<serde_json::Value> = d
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::OperatorNote)
        .map(|m| serde_json::json!({ "message": m.id, "text": m.payload["text"], "delivery": m.delivery, "delivered_at_ms": m.delivered_at_ms }))
        .collect();
    let accepted = d
        .messages
        .iter()
        .rev()
        .find(|m| m.sender == "agent" && m.kind != MessageKind::MalformedReport)
        .map(|m| serde_json::json!({ "kind": m.kind, "payload": m.payload }));
    let repair_demonstrated = !rejected.is_empty() && accepted.is_some();
    let summary = serde_json::json!({
        "task": task,
        "session": session,
        "rejections": rejected,
        "repair_notes": notes,
        "accepted_report": accepted,
        "repair_demonstrated": repair_demonstrated,
        "all_agent_messages": agent_messages.iter().map(|(seq, at, text)| serde_json::json!({"seq": seq, "at": at, "text": text})).collect::<Vec<_>>(),
    });
    let _ = std::fs::write(ev.root().join("repair.json"), serde_json::to_string_pretty(&summary).unwrap());
    ev.log(&format!(
        "rejections {}, repair notes {}, accepted report {}, report repair demonstrated: {repair_demonstrated}",
        summary["rejections"].as_array().map_or(0, Vec::len),
        summary["repair_notes"].as_array().map_or(0, Vec::len),
        summary["accepted_report"]["kind"]
    ));
    ev.passed();
    summary
}

fn panic_text(e: tokio::task::JoinError) -> String {
    match e.try_into_panic() {
        Ok(p) => match p.downcast::<String>() {
            Ok(m) => *m,
            Err(p) => {
                p.downcast::<&str>().map(|m| (*m).to_owned()).unwrap_or_else(|_| "non-string panic".into())
            }
        },
        Err(e) => format!("task did not finish: {e}"),
    }
}

/// Sessions, Claude-reported cost and Codex tokens per task, from Core's database.
fn usage_by_task(dir: &Path) -> serde_json::Value {
    let db = read_db(dir);
    let mut st = db
        .prepare(
            "SELECT a.task_id, a.role, a.provider, a.status, a.outcome, s.id, \
               (SELECT COUNT(*) FROM events e WHERE e.session_id = s.id AND e.event_type = 'agent.turn_completed'), \
               (SELECT SUM(json_extract(e.payload, '$.turn_cost_usd')) FROM events e \
                  WHERE e.session_id = s.id AND e.event_type = 'agent.turn_completed'), \
               (SELECT SUM(json_extract(e.payload, '$.usage.total_tokens')) FROM events e \
                  WHERE e.session_id = s.id AND e.event_type = 'agent.turn_completed'), \
               (SELECT MIN(e.occurred_at) FROM events e WHERE e.session_id = s.id), \
               (SELECT MAX(e.occurred_at) FROM events e WHERE e.session_id = s.id) \
             FROM task_assignments a LEFT JOIN sessions s ON s.id = a.session_id ORDER BY a.created_at",
        )
        .unwrap();
    let rows: Vec<serde_json::Value> = st
        .query_map([], |r| {
            Ok(serde_json::json!({
                "task": r.get::<_, String>(0)?, "role": r.get::<_, String>(1)?, "provider": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?, "outcome": r.get::<_, Option<String>>(4)?,
                "session": r.get::<_, Option<String>>(5)?, "turns": r.get::<_, i64>(6)?,
                "claude_cost_usd": r.get::<_, Option<f64>>(7)?, "codex_tokens": r.get::<_, Option<i64>>(8)?,
                "first_event_ms": r.get::<_, Option<i64>>(9)?, "last_event_ms": r.get::<_, Option<i64>>(10)?,
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let sessions: i64 = db.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
    serde_json::json!({ "sessions_in_core": sessions, "assignments": rows })
}

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn correction_cycle_and_report_repair_with_real_agents() {
    let Some(mut core) = TestCore::start_real(RealCli { claude: true, codex: true }, |cfg| {
        cfg.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
    else {
        return;
    };
    core.dir.disable_cleanup(true);
    let dir = core.dir.path().to_path_buf();
    println!("experiment directory (kept): {}", dir.display());
    let started = std::time::Instant::now();

    // Experiment 1 (at most 5 sessions).
    let (c1, _rx1) = core.connect().await;
    let exp1 = match tokio::spawn(experiment1(c1, dir.clone())).await {
        Ok(s) => serde_json::json!({ "outcome": "PASSED", "summary": s }),
        Err(e) => serde_json::json!({ "outcome": "NOT PASSED", "reason": panic_text(e) }),
    };
    // Core and evidence must be healthy to continue.
    let (probe, _rxp) = core.connect().await;
    let core_ok = probe.call::<SystemHealth>(Empty {}).await.is_ok();
    let db_ok = read_db(&dir).query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).ok().as_deref()
        == Some("ok");
    let evidence_ok = dir.join("evidence-exp1/termination.json").exists();
    let health = serde_json::json!({ "core_responds": core_ok, "database_quick_check_ok": db_ok, "exp1_evidence_complete": evidence_ok });
    let mut summary = serde_json::json!({ "experiment1": exp1, "health_after_experiment1": health });
    let write = |s: &serde_json::Value| {
        let _ = std::fs::write(dir.join("SUMMARY.json"), serde_json::to_string_pretty(s).unwrap());
    };
    if !(core_ok && db_ok && evidence_ok) {
        summary["usage"] = usage_by_task(&dir);
        write(&summary);
        panic!("STOPPED after experiment 1: Core or evidence integrity is not intact: {health}");
    }

    // Experiment 2 (1 session).
    let (c2, _rx2) = core.connect().await;
    summary["experiment2"] = match tokio::spawn(experiment2(c2, dir.clone())).await {
        Ok(s) => serde_json::json!({ "outcome": "PASSED", "summary": s }),
        Err(e) => serde_json::json!({ "outcome": "NOT PASSED", "reason": panic_text(e) }),
    };
    summary["usage"] = usage_by_task(&dir);
    summary["duration_secs"] = started.elapsed().as_secs().into();
    write(&summary);
    println!("SUMMARY: {}", serde_json::to_string_pretty(&summary).unwrap());
    let sessions = summary["usage"]["sessions_in_core"].as_i64().unwrap_or(99);
    assert!(sessions <= 6, "session cap: {sessions}");
    assert_eq!(summary["experiment1"]["outcome"], "PASSED", "experiment 1");
    assert_eq!(summary["experiment2"]["outcome"], "PASSED", "experiment 2");
}

/// Both experiments' setups without agents: the experiment 1 check fails on
/// the original implementation and passes on a correct one (so it measures
/// the requirements, not itself), and experiment 2's task carries the
/// disclosed convention.
#[tokio::test]
async fn experiment_setups_without_agents() {
    if !Path::new(ORIGINAL_RUN).join(ORIGINAL_IMPL).exists() {
        eprintln!("skipped: the preserved failed run is not on this machine");
        return;
    }
    let core = TestCore::start_with(|_| {}).await;
    let dir = core.dir.path().to_path_buf();
    let (c, _rx) = core.connect().await;
    let ev1 = Evidence::named(&dir, "evidence-exp1");
    let ((_r, project, check, base, ws), pre) =
        seed_replay(&dir, &c, &ev1, "replay-exp1", exp1_check()).await;
    assert!(pre.contains("ValueError") || pre.contains("AssertionError"), "{pre}");
    let reference = dir.join("reference");
    std::fs::create_dir_all(&reference).unwrap();
    std::fs::write(reference.join("durations.py"), EXP1_REFERENCE).unwrap();
    std::fs::write(reference.join("check.py"), exp1_check()).unwrap();
    let out =
        Command::new("/usr/bin/python3").args(["-B", "check.py"]).current_dir(&reference).output().unwrap();
    assert!(
        out.status.success(),
        "the check passes a correct implementation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("all 10 valid and 20 invalid inputs behave as required")
    );
    // Through Core, on the original implementation: fails.
    c.call::<IntegrationsSetSettings>(IntegrationsSetSettingsParams {
        project_id: project,
        require_review: false,
    })
    .await
    .unwrap();
    let v = replay_validate(&c, ws, check, None, &ev1).await;
    assert_eq!(v.runs[0].status, RunStatus::Failed);
    assert_eq!(git(&dir.join("replay-exp1"), &["rev-parse", "main"]), base);
    // Experiment 2's setup: its own clone, workspace and evidence.
    let ev2 = Evidence::named(&dir, "evidence-exp2");
    let ((_r2, project2, _c2, _b2, _ws2), _) =
        seed_replay(&dir, &c, &ev2, "replay-exp2", replay_check()).await;
    let task = exp2_task(&c, project2).await;
    let t = c.call::<TasksGet>(TaskIdParams { task_id: task }).await.unwrap().task;
    assert!(t.description.as_deref().unwrap().ends_with(EXP2_CONVENTION));
    assert!(dir.join("evidence-exp1/replay").exists() && dir.join("evidence-exp2/replay").exists());
    assert_eq!(
        sha1(&Path::new(ORIGINAL_RUN).join(ORIGINAL_IMPL)),
        ORIGINAL_IMPL_SHA1,
        "the original is untouched"
    );
}
