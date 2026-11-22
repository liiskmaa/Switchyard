//! Arguments, environment and the admin policy for `gemini --acp`.
//!
//! Verified against Gemini CLI 0.70.0 (docs/architecture/acp-integration.md):
//! * `--admin-policy <dir>` loads [`POLICY_TOML`] at the highest policy tier,
//!   above the user's and the repository's policies;
//! * every file and shell tool asks the client (`ask_user`); the shell rule
//!   has an `argsPattern`, without which Gemini turns "ask" into "allow" for
//!   commands it judges safe (`ls`, `pwd`, `echo`, …);
//! * tools that would act outside Switchyard (web, sub-agents, MCP, skills,
//!   plan-mode switches, Gemini's own question to the user) are denied;
//! * `GEMINI_CLI_HOME` points at a Switchyard directory, so the user's
//!   Gemini settings, extensions and MCP servers never apply; the repository
//!   is not trusted, so its `.gemini` settings and hooks are ignored.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::process::{EnvPolicy, prepend_path};

/// The login Switchyard selects: the Gemini API key (the personal Google
/// login no longer works for Gemini CLI).
pub const AUTH_METHOD: &str = "gemini-api-key";
pub const API_KEY_VAR: &str = "GEMINI_API_KEY";

/// The admin-tier policy (file `switchyard.toml` in the policy directory).
pub const POLICY_TOML: &str = r#"# Written by Switchyard. Every tool call that touches files or runs a
# command asks Switchyard (ACP session/request_permission); Switchyard's
# policies decide. Tools that would act outside Switchyard are denied.

[[rule]]
toolName = "*"
decision = "ask_user"
priority = 900

# The pattern keeps Gemini's "known safe command" heuristic from turning
# ask into allow (verified with 0.70.0).
[[rule]]
toolName = "run_shell_command"
argsPattern = ".*"
decision = "ask_user"
priority = 999

[[rule]]
toolName = [
  "web_fetch",
  "google_web_search",
  "invoke_agent",
  "complete_task",
  "activate_skill",
  "take_snapshot",
  "list_mcp_resources",
  "read_mcp_resource",
  "enter_plan_mode",
  "exit_plan_mode",
  "ask_user",
  "tracker_create_task",
  "tracker_update_task",
  "tracker_get_task",
  "tracker_list_tasks",
  "tracker_add_dependency",
  "tracker_visualize",
]
decision = "deny"
priority = 999

# Internal bookkeeping with no effect outside the conversation.
[[rule]]
toolName = ["update_topic", "write_todos"]
decision = "allow"
priority = 999
"#;

/// Printed on stderr when a policy file does not load. Gemini then runs
/// without it (fails open), so Switchyard stops the session.
pub const POLICY_ERROR_MARKER: &str = "Policy file error";

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// `Some(session id)` to resume; `None` for a new session.
    pub resume_session: Option<String>,
    /// Switchyard's directory for Gemini: its home and the policy.
    pub state_dir: PathBuf,
}

impl LaunchSpec {
    pub fn policy_dir(&self) -> PathBuf {
        self.state_dir.join("policy")
    }
    pub fn home_dir(&self) -> PathBuf {
        self.state_dir.join("home")
    }
}

/// Writes the policy and creates the isolated home. Idempotent.
pub fn prepare(spec: &LaunchSpec) -> std::io::Result<()> {
    std::fs::create_dir_all(spec.policy_dir())?;
    std::fs::create_dir_all(spec.home_dir())?;
    let path = spec.policy_dir().join("switchyard.toml");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(POLICY_TOML) {
        std::fs::write(&path, POLICY_TOML)?;
    }
    // An empty trusted-folders file: no folder is trusted.
    let trusted = spec.home_dir().join("trustedFolders.json");
    if !trusted.exists() {
        std::fs::write(trusted, "{}")?;
    }
    Ok(())
}

pub fn build_args(spec: &LaunchSpec) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--acp".into(), "--admin-policy".into(), spec.policy_dir().into()];
    if let Some(m) = &spec.model {
        args.push("-m".into());
        args.push(m.into());
    }
    args
}

pub const ENV_POLICY: EnvPolicy = EnvPolicy {
    allow: &[
        "USER",
        "LOGNAME",
        "PATH",
        "SHELL",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "HOME",
        API_KEY_VAR,
    ],
    allow_prefixes: &[],
    deny: &["GEMINI_CLI_*", "GOOGLE_*", "GEMINI_SANDBOX*"],
};

/// The key from the user's `~/.gemini/.env` when the environment has none
/// (the desktop app does not inherit a shell's variables). Never logged.
pub fn api_key_from_dotenv(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(".gemini").join(".env")).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim().strip_prefix("export ").unwrap_or(l.trim());
        let v = l.strip_prefix(API_KEY_VAR)?.trim_start().strip_prefix('=')?.trim();
        let v = v.trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_owned())
    })
}

pub fn build_env(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    executable: &Path,
    spec: &LaunchSpec,
) -> Vec<(OsString, OsString)> {
    let mut env = ENV_POLICY.build(parent);
    if let Some(dir) = executable.parent() {
        prepend_path(&mut env, dir);
    }
    let has_key = env.iter().any(|(k, v)| k == API_KEY_VAR && !v.is_empty());
    if !has_key {
        let home = env.iter().find(|(k, _)| k == "HOME").map(|(_, v)| PathBuf::from(v));
        if let Some(key) = home.as_deref().and_then(api_key_from_dotenv) {
            env.push((API_KEY_VAR.into(), key.into()));
        }
    }
    env.push(("GEMINI_CLI_HOME".into(), spec.home_dir().into()));
    env.push(("GEMINI_CLI_TRUSTED_FOLDERS_PATH".into(), spec.home_dir().join("trustedFolders.json").into()));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            cwd: "/w".into(),
            model: Some("gemini-3.5-flash-lite".into()),
            resume_session: None,
            state_dir: "/data/gemini".into(),
        }
    }

    #[test]
    fn arguments_load_the_admin_policy_and_never_weaken_safety() {
        let args: Vec<String> = build_args(&spec()).into_iter().map(|a| a.into_string().unwrap()).collect();
        assert_eq!(args, ["--acp", "--admin-policy", "/data/gemini/policy", "-m", "gemini-3.5-flash-lite"]);
        for bad in ["yolo", "approval-mode", "auto_edit", "skip-trust", "sandbox"] {
            assert!(!args.iter().any(|a| a.contains(bad)), "{bad}");
        }
    }

    #[test]
    fn the_policy_asks_for_shell_with_a_pattern_and_denies_outside_tools() {
        assert!(POLICY_TOML.contains(
            "toolName = \"run_shell_command\"\nargsPattern = \".*\"\ndecision = \"ask_user\"\npriority = 999"
        ));
        for t in ["web_fetch", "google_web_search", "invoke_agent", "ask_user", "enter_plan_mode"] {
            assert!(POLICY_TOML.contains(&format!("\"{t}\"")), "{t}");
        }
    }

    #[test]
    fn environment_is_filtered_isolated_and_carries_the_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".gemini")).unwrap();
        std::fs::write(dir.path().join(".gemini/.env"), "# x\nexport GEMINI_API_KEY=\"k-123\"\n").unwrap();
        let parent = [
            ("HOME", dir.path().to_str().unwrap()),
            ("PATH", "/usr/bin"),
            ("GEMINI_CLI_HOME", "/elsewhere"),
            ("GOOGLE_CLOUD_PROJECT", "p"),
            ("GITHUB_TOKEN", "secret"),
            ("OPENAI_API_KEY", "o"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = build_env(parent, Path::new("/opt/gemini/bin/gemini"), &spec());
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_str().unwrap().to_owned());
        assert_eq!(get("GEMINI_API_KEY").as_deref(), Some("k-123"));
        assert_eq!(get("GEMINI_CLI_HOME").as_deref(), Some("/data/gemini/home"));
        assert_eq!(get("PATH").as_deref(), Some("/opt/gemini/bin:/usr/bin"));
        for gone in ["GITHUB_TOKEN", "OPENAI_API_KEY", "GOOGLE_CLOUD_PROJECT"] {
            assert!(get(gone).is_none(), "{gone}");
        }
        assert_eq!(env.iter().filter(|(k, _)| k == "GEMINI_CLI_HOME").count(), 1);
    }
}
