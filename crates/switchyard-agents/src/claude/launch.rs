//! Command line and environment for a Claude Code session.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::process::{EnvPolicy, prepend_path};

/// Built-in tools enabled for sessions. `--restricted` removes code-running
/// tools unless named here; `Task`, web tools and harness-specific tools are
/// deliberately excluded until Switchyard supervises them.
pub const TOOLS: &str = "Read,Write,Edit,Glob,Grep,Bash,NotebookEdit";

/// Settings layered on top of `--restricted`. The `ask` rule routes *every*
/// Bash command — including Claude's built-in read-only commands such as
/// `cat` — to Switchyard (verified with CLI 2.1.322).
pub const SETTINGS: &str = r#"{"permissions":{"ask":["Bash"],"defaultMode":"default"}}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeSession {
    /// Start a new conversation with this id (`--session-id`).
    New(Uuid),
    /// Continue an existing conversation (`--resume`).
    Resume(String),
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub session: NativeSession,
    pub cwd: PathBuf,
    pub model: Option<String>,
}

pub fn build_args(spec: &LaunchSpec) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "default",
        "--permission-prompt-tool",
        "stdio",
        "--restricted",
        "--strict-mcp-config",
        "--tools",
        TOOLS,
        "--settings",
        SETTINGS,
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    match &spec.session {
        NativeSession::New(id) => {
            args.push("--session-id".into());
            args.push(id.to_string().into());
        }
        NativeSession::Resume(id) => {
            args.push("--resume".into());
            args.push(id.into());
        }
    }
    if let Some(model) = &spec.model {
        args.push("--model".into());
        args.push(model.into());
    }
    args
}

/// Environment passed to Claude: enough for the CLI, its auth (keychain via
/// HOME, or provider variables) and the tools it runs — nothing else.
/// Variables set by an enclosing Claude Code session are removed so the
/// child does not believe it is nested.
pub const ENV_POLICY: EnvPolicy = EnvPolicy {
    allow: &[
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "SHELL",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "SSH_AUTH_SOCK",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "CLOUD_ML_REGION",
        "CLAUDE_CONFIG_DIR",
    ],
    allow_prefixes: &["ANTHROPIC_", "AWS_", "CLAUDE_CODE_USE_", "VERTEX_REGION_", "XDG_"],
    deny: &["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SESSION*", "CLAUDE_CODE_MESSAGING*"],
};

pub fn build_env(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    executable: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env = ENV_POLICY.build(parent);
    if let Some(dir) = executable.parent() {
        prepend_path(&mut env, dir);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(spec: &LaunchSpec) -> Vec<String> {
        build_args(spec).into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn new_session_arguments() {
        let id = Uuid::now_v7();
        let args = args_of(&LaunchSpec { session: NativeSession::New(id), cwd: "/r".into(), model: None });
        assert_eq!(args[0], "-p");
        for pair in [
            ["--input-format", "stream-json"],
            ["--output-format", "stream-json"],
            ["--permission-prompt-tool", "stdio"],
            ["--permission-mode", "default"],
            ["--tools", TOOLS],
            ["--settings", SETTINGS],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "missing {pair:?}");
        }
        assert!(args.contains(&"--restricted".into()) && args.contains(&"--strict-mcp-config".into()));
        assert!(args.windows(2).any(|w| w == ["--session-id".to_owned(), id.to_string()]));
        assert!(!args.iter().any(|a| a.contains("dangerously") || a == "bypassPermissions"));
        assert!(!args.contains(&"--resume".into()) && !args.contains(&"--model".into()));
    }

    #[test]
    fn resume_arguments_and_model() {
        let args = args_of(&LaunchSpec {
            session: NativeSession::Resume("abc".into()),
            cwd: "/r".into(),
            model: Some("sonnet".into()),
        });
        assert!(args.windows(2).any(|w| w == ["--resume", "abc"]));
        assert!(args.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(!args.contains(&"--session-id".into()));
    }

    #[test]
    fn settings_are_valid_json_with_bash_ask_rule() {
        let v: serde_json::Value = serde_json::from_str(SETTINGS).unwrap();
        assert_eq!(v["permissions"]["ask"][0], "Bash");
    }

    #[test]
    fn environment_drops_nesting_markers_and_unrelated_secrets() {
        let parent = [
            ("HOME", "/h"),
            ("PATH", "/usr/bin"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "x"),
            ("GITHUB_TOKEN", "secret"),
            ("ANTHROPIC_API_KEY", "k"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = build_env(parent, Path::new("/opt/claude/bin/claude"));
        let names: Vec<_> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert_eq!(names, ["ANTHROPIC_API_KEY", "HOME", "PATH"]);
        assert_eq!(env[2].1, OsString::from("/opt/claude/bin:/usr/bin"));
    }
}
