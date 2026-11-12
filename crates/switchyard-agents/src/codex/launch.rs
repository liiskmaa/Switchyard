//! Arguments and environment for `codex app-server`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::process::{EnvPolicy, prepend_path};

/// Features disabled for Switchyard sessions: tools that act without an
/// approval request (ADR 0021). Verified names for codex-cli 0.166.0.
pub const DISABLED_FEATURES: &[&str] = &[
    "apps",
    "plugins",
    "remote_plugin",
    "multi_agent",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "in_app_browser",
    "image_generation",
    "hooks",
    "goals",
];

/// Configuration overrides (`-c key=value`, TOML values).
pub const CONFIG_OVERRIDES: &[&str] = &[
    // No web search tool.
    "web_search=\"disabled\"",
    // No MCP servers, including ones from the user's config.toml.
    "mcp_servers={}",
];

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// `Some(thread id)` to resume; `None` for a new thread.
    pub resume_thread: Option<String>,
}

pub fn build_args() -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["app-server".into(), "--listen".into(), "stdio://".into()];
    for f in DISABLED_FEATURES {
        args.push("--disable".into());
        args.push((*f).into());
    }
    for c in CONFIG_OVERRIDES {
        args.push("-c".into());
        args.push((*c).into());
    }
    args
}

/// The parent's environment, filtered. Variables set by an enclosing Codex
/// session (sandbox markers, thread ids) are removed.
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
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "CODEX_HOME",
        "CODEX_CA_CERTIFICATE",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_ORGANIZATION",
        "OPENAI_PROJECT",
    ],
    allow_prefixes: &["XDG_"],
    deny: &["CODEX_SANDBOX*", "CODEX_THREAD_ID", "CODEX_INTERNAL*"],
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

    #[test]
    fn arguments_disable_unmediated_tools_and_never_weaken_safety() {
        let args: Vec<String> = build_args().into_iter().map(|a| a.into_string().unwrap()).collect();
        assert_eq!(&args[..3], ["app-server", "--listen", "stdio://"]);
        for f in ["plugins", "apps", "multi_agent", "computer_use", "hooks"] {
            assert!(args.windows(2).any(|w| w == ["--disable", f]), "{f}");
        }
        assert!(args.windows(2).any(|w| w == ["-c", "mcp_servers={}"]));
        assert!(args.windows(2).any(|w| w == ["-c", "web_search=\"disabled\""]));
        for bad in ["danger", "never", "bypass", "full-access", "--enable", "experimental"] {
            assert!(!args.iter().any(|a| a.contains(bad)), "{bad}");
        }
    }

    #[test]
    fn environment_is_filtered() {
        let parent = [
            ("HOME", "/h"),
            ("PATH", "/usr/bin"),
            ("CODEX_SANDBOX", "seatbelt"),
            ("CODEX_THREAD_ID", "x"),
            ("GITHUB_TOKEN", "secret"),
            ("ANTHROPIC_API_KEY", "k"),
            ("OPENAI_API_KEY", "o"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = build_env(parent, Path::new("/opt/codex/bin/codex"));
        let names: Vec<_> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert_eq!(names, ["HOME", "OPENAI_API_KEY", "PATH"]);
        assert_eq!(env[2].1, OsString::from("/opt/codex/bin:/usr/bin"));
    }
}
