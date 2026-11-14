//! Project validation commands (ADR 0030): a local runner, not CI.
//!
//! A command is an argument vector run directly (never through a shell),
//! in the workspace, after the policy evaluator allowed it or the operator
//! authorized it.

use serde::{Deserialize, Serialize};

use crate::model::string_enum;

/// Output kept per stream; the rest is cut (and marked truncated).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_ARGS: usize = 64;
const MAX_ARG: usize = 4096;
const MAX_TIMEOUT_SECS: u32 = 4 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationCommand {
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0}")]
    Invalid(String),
}

/// Checks a configured command.
pub fn validate_command(c: &ValidationCommand) -> Result<(), ValidationError> {
    let bad = |m: &str| Err(ValidationError::Invalid(m.into()));
    if c.name.trim().is_empty() || c.name.chars().count() > 100 {
        return bad("the name must be 1 to 100 characters");
    }
    if c.argv.is_empty() || c.argv[0].trim().is_empty() {
        return bad("the command needs a program");
    }
    if c.argv.len() > MAX_ARGS || c.argv.iter().any(|a| a.len() > MAX_ARG || a.contains('\0')) {
        return bad("too many or too long arguments");
    }
    if c.timeout_secs == 0 || c.timeout_secs > MAX_TIMEOUT_SECS {
        return bad("the timeout must be between 1 second and 4 hours");
    }
    Ok(())
}

/// The command as one line, shell-quoted, for policy matching and display.
/// It is never passed to a shell.
pub fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

string_enum!(
    /// A validation run for one source commit.
    RunStatus {
        Running => "running",
        Passed => "passed",
        Failed => "failed",
        Cancelled => "cancelled",
        /// Core stopped (or restarted) while it ran.
        Interrupted => "interrupted",
    }
);

string_enum!(
    /// One command of a run.
    StepStatus {
        Pending => "pending",
        Running => "running",
        Passed => "passed",
        Failed => "failed",
        TimedOut => "timed_out",
        Cancelled => "cancelled",
        /// The policy denied it, or it needed the operator's authorization
        /// and did not have it. It never ran.
        Refused => "refused",
        /// Not run because an earlier command failed or the run was stopped.
        Skipped => "skipped",
        /// The process could not be started.
        Error => "error",
    }
);

impl StepStatus {
    pub fn is_final(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(argv: &[&str]) -> ValidationCommand {
        ValidationCommand {
            name: "t".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            timeout_secs: 60,
        }
    }

    #[test]
    fn commands_are_checked() {
        assert!(validate_command(&cmd(&["cargo", "test"])).is_ok());
        assert!(validate_command(&cmd(&[])).is_err());
        assert!(validate_command(&cmd(&[" "])).is_err());
        assert!(validate_command(&ValidationCommand { timeout_secs: 0, ..cmd(&["x"]) }).is_err());
        assert!(validate_command(&cmd(&["a", "b\0c"])).is_err());
    }

    #[test]
    fn the_command_line_quotes_what_a_shell_would_split() {
        assert_eq!(command_line(&["cargo".into(), "test".into(), "--all".into()]), "cargo test --all");
        assert_eq!(command_line(&["sh".into(), "-c".into(), "a; b".into()]), "sh -c 'a; b'");
        assert_eq!(command_line(&["echo".into(), "it's".into()]), "echo 'it'\\''s'");
    }
}

/// The final status of a validation run. `observed` is what running the
/// steps produced; a run is `Passed` only if it had at least one step and
/// every step's success was both observed and recorded (`confirmed` counts
/// those). Anything less is `Failed` (or the observed failure/cancellation).
pub fn final_run_status(expected: usize, confirmed: usize, observed: RunStatus) -> RunStatus {
    match observed {
        RunStatus::Passed if expected == 0 || confirmed != expected => RunStatus::Failed,
        other => other,
    }
}

#[cfg(test)]
mod final_status_tests {
    use super::*;

    #[test]
    fn a_run_passes_only_with_every_step_confirmed() {
        assert_eq!(final_run_status(0, 0, RunStatus::Passed), RunStatus::Failed, "zero commands");
        assert_eq!(final_run_status(3, 2, RunStatus::Passed), RunStatus::Failed, "partial execution");
        assert_eq!(final_run_status(3, 3, RunStatus::Passed), RunStatus::Passed, "all executed and recorded");
        assert_eq!(final_run_status(3, 3, RunStatus::Cancelled), RunStatus::Cancelled);
        assert_eq!(final_run_status(3, 1, RunStatus::Failed), RunStatus::Failed);
    }
}
