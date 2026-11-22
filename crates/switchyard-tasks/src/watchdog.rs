//! The stuck-work watchdog (ADR 0045), without I/O: from a snapshot of one
//! task's workflow or team, its open step and its recent history, decide
//! whether it is stuck, why, and what to do.
//!
//! Core gathers the snapshot from its own records (never from an agent's
//! say-so), runs [`check`] periodically, journals changes, and performs only
//! the safe [`Remedy`]s itself; everything else is shown to the operator.

use serde::{Deserialize, Serialize};

/// When something counts as stuck. [`Limits::default`] is what Core uses;
/// tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Resumes that bounced back to the same wait within this long count.
    pub bounce_window_ms: i64,
    /// How many bounces make it stuck.
    pub bounces: usize,
    /// Only bounces this recent count.
    pub bounce_lookback_ms: i64,
    /// An active step whose session is gone for this long is orphaned.
    pub orphan_grace_ms: i64,
    /// No agent activity for this long while working.
    pub silent_ms: i64,
    /// A pending approval this old gets a reminder.
    pub approval_wait_ms: i64,
    /// A question to the operator this old gets a reminder.
    pub question_wait_ms: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bounce_window_ms: 10_000,
            bounces: 2,
            bounce_lookback_ms: 30 * 60_000,
            orphan_grace_ms: 60_000,
            silent_ms: 20 * 60_000,
            approval_wait_ms: 10 * 60_000,
            question_wait_ms: 30 * 60_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An engineering workflow.
    Workflow,
    /// An agent organization (team).
    Team,
}

/// A resume and what followed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    pub at_ms: i64,
    /// When it was waiting again, with which reason (none: it kept running).
    pub waiting_again: Option<(i64, String)>,
}

/// The task's open step, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStep {
    pub key: String,
    /// The step's session is attached to a running process.
    pub session_alive: bool,
    /// When the step started (or its session last changed state).
    pub since_ms: i64,
    /// The latest agent activity of its session.
    pub last_activity_ms: Option<i64>,
}

/// What Core knows about one task's workflow or team now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub kind: Kind,
    /// `running`, `waiting`, `paused`, …
    pub status: String,
    pub reason: Option<String>,
    /// When the current wait began.
    pub waiting_since_ms: Option<i64>,
    /// A question to the operator is open.
    pub question: bool,
    pub open_step: Option<OpenStep>,
    /// The oldest pending approval of the task's sessions.
    pub oldest_approval_ms: Option<i64>,
    /// Recent resumes, oldest first.
    pub resumes: Vec<Resume>,
}

/// Why the work is stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    ResumeBounces,
    OrphanedStep,
    SilentAgent,
    ApprovalWaiting,
    QuestionWaiting,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResumeBounces => "resume_bounces",
            Self::OrphanedStep => "orphaned_step",
            Self::SilentAgent => "silent_agent",
            Self::ApprovalWaiting => "approval_waiting",
            Self::QuestionWaiting => "question_waiting",
        }
    }
}

/// What Core does by itself. Only these; everything else is the operator's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// Nothing automatic.
    None,
    /// A team: hand the reason to its lead (once per reason).
    TellLead(String),
    /// A workflow step whose session is gone: treat it as interrupted, so
    /// Resume runs it again (the same as after a Core restart).
    InterruptStep(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stuck {
    pub code: Code,
    /// One line for a list.
    pub title: String,
    /// What is wrong and what the operator can do.
    pub detail: String,
    pub since_ms: i64,
    pub remedy: Remedy,
}

fn minutes(ms: i64) -> i64 {
    (ms / 60_000).max(1)
}

/// The most important reason the task is stuck now, if it is.
pub fn check(s: &Snapshot, now_ms: i64, l: &Limits) -> Option<Stuck> {
    // 1. A step whose agent is gone: nothing will ever report.
    if let Some(step) = &s.open_step
        && s.status == "running"
        && !step.session_alive
        && now_ms - step.since_ms >= l.orphan_grace_ms
    {
        let remedy = match s.kind {
            Kind::Workflow => Remedy::InterruptStep(step.key.clone()),
            Kind::Team => Remedy::None,
        };
        let detail = match s.kind {
            Kind::Workflow => format!(
                "The step {} is open but its agent session ended (a crash or a restart). Switchyard ends the step; \
                 resume to run it again.",
                step.key
            ),
            Kind::Team => format!(
                "The team's current turn ({}) has no running agent session (a crash, a restart, or a start that \
                 failed). Stop the team and start it again, or restart Core and resume: Core then settles the \
                 interrupted turn.",
                step.key
            ),
        };
        return Some(Stuck {
            code: Code::OrphanedStep,
            title: format!("{}'s agent is not running", step.key),
            detail,
            since_ms: step.since_ms,
            remedy,
        });
    }
    // 2. Resume keeps bouncing back to the same wait.
    let recent: Vec<&Resume> =
        s.resumes.iter().filter(|r| now_ms - r.at_ms <= l.bounce_lookback_ms).collect();
    let bounces: Vec<&(i64, String)> = recent
        .iter()
        .filter_map(|r| r.waiting_again.as_ref().filter(|(at, _)| at - r.at_ms <= l.bounce_window_ms))
        .collect();
    if s.status == "waiting"
        && let Some(reason) = s.reason.as_deref()
    {
        let same = bounces.iter().filter(|(_, why)| why == reason).count();
        if same >= l.bounces {
            let first = recent.first().map_or(now_ms, |r| r.at_ms);
            let (detail, remedy) = match s.kind {
                Kind::Team => (
                    format!(
                        "Resuming the team {same} times changed nothing: \u{201c}{reason}\u{201d} Switchyard handed this \
                         to the lead to decide; you can also tell it how to continue in Ask the supervisor."
                    ),
                    Remedy::TellLead(reason.to_owned()),
                ),
                Kind::Workflow => (
                    format!(
                        "Resuming {same} times changed nothing: \u{201c}{reason}\u{201d} Resume will not help here. Ask \
                         the supervisor to route the work, take the task over, or stop the workflow."
                    ),
                    Remedy::None,
                ),
            };
            return Some(Stuck {
                code: Code::ResumeBounces,
                title: "Resume doesn't help".into(),
                detail,
                since_ms: first,
                remedy,
            });
        }
    }
    // 3. Working, but silent.
    if let Some(step) = &s.open_step
        && s.status == "running"
        && step.session_alive
    {
        let last = step.last_activity_ms.unwrap_or(step.since_ms);
        if now_ms - last >= l.silent_ms {
            return Some(Stuck {
                code: Code::SilentAgent,
                title: format!("{} has been silent for {} min", step.key, minutes(now_ms - last)),
                detail:
                    "The agent shows no activity. Open its session to see what it is doing, send it a note, \
                         or interrupt it."
                        .into(),
                since_ms: last,
                remedy: Remedy::None,
            });
        }
    }
    // 4. Waiting for the operator for a long time.
    if let Some(at) = s.oldest_approval_ms
        && now_ms - at >= l.approval_wait_ms
    {
        return Some(Stuck {
            code: Code::ApprovalWaiting,
            title: format!("An approval has waited {} min", minutes(now_ms - at)),
            detail:
                "An agent is waiting for your approval; nothing moves until you decide (bottom-right panel)."
                    .into(),
            since_ms: at,
            remedy: Remedy::None,
        });
    }
    if s.question
        && s.status == "waiting"
        && let Some(at) = s.waiting_since_ms
        && now_ms - at >= l.question_wait_ms
    {
        return Some(Stuck {
            code: Code::QuestionWaiting,
            title: format!("A question has waited {} min", minutes(now_ms - at)),
            detail: "The agent asked you something and waits for your answer under Needs you.".into(),
            since_ms: at,
            remedy: Remedy::None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 10 * 3_600_000;

    fn base(kind: Kind, status: &str) -> Snapshot {
        Snapshot {
            kind,
            status: status.into(),
            reason: None,
            waiting_since_ms: None,
            question: false,
            open_step: None,
            oldest_approval_ms: None,
            resumes: vec![],
        }
    }

    fn bounce(at: i64, reason: &str) -> Resume {
        Resume { at_ms: at, waiting_again: Some((at + 500, reason.into())) }
    }

    #[test]
    fn repeated_resumes_that_change_nothing_are_stuck() {
        let mut s = base(Kind::Team, "waiting");
        s.reason = Some("Nothing can run.".into());
        s.resumes = vec![bounce(NOW - 60_000, "Nothing can run.")];
        assert_eq!(check(&s, NOW, &Limits::default()), None, "once is not a pattern");
        s.resumes.push(bounce(NOW - 30_000, "Nothing can run."));
        let st = check(&s, NOW, &Limits::default()).unwrap();
        assert_eq!(st.code, Code::ResumeBounces);
        assert_eq!(st.remedy, Remedy::TellLead("Nothing can run.".into()));
        // A workflow gets no automatic remedy, only the honest options.
        s.kind = Kind::Workflow;
        let st = check(&s, NOW, &Limits::default()).unwrap();
        assert_eq!(st.remedy, Remedy::None);
        assert!(st.detail.contains("Resume will not help"));
        // Bounces with another reason, old ones, or resumes that kept running do not count.
        s.resumes = vec![
            bounce(NOW - 60_000, "something else"),
            bounce(NOW - 2 * 3_600_000, "Nothing can run."),
            Resume { at_ms: NOW - 10_000, waiting_again: None },
        ];
        assert_eq!(check(&s, NOW, &Limits::default()), None);
    }

    #[test]
    fn a_step_whose_agent_is_gone_is_interrupted_after_a_grace_period() {
        let mut s = base(Kind::Workflow, "running");
        s.open_step = Some(OpenStep {
            key: "implement#1".into(),
            session_alive: false,
            since_ms: NOW - 30_000,
            last_activity_ms: None,
        });
        assert_eq!(check(&s, NOW, &Limits::default()), None, "within the grace period");
        s.open_step.as_mut().unwrap().since_ms = NOW - 120_000;
        let st = check(&s, NOW, &Limits::default()).unwrap();
        assert_eq!(
            (st.code, st.remedy.clone()),
            (Code::OrphanedStep, Remedy::InterruptStep("implement#1".into()))
        );
        s.kind = Kind::Team;
        assert_eq!(check(&s, NOW, &Limits::default()).unwrap().remedy, Remedy::None);
    }

    #[test]
    fn silence_and_long_waits_are_reported() {
        let mut s = base(Kind::Workflow, "running");
        s.open_step = Some(OpenStep {
            key: "review#1".into(),
            session_alive: true,
            since_ms: NOW - 3_600_000,
            last_activity_ms: Some(NOW - 25 * 60_000),
        });
        let st = check(&s, NOW, &Limits::default()).unwrap();
        assert_eq!(st.code, Code::SilentAgent);
        assert!(st.title.contains("25 min"));
        s.open_step.as_mut().unwrap().last_activity_ms = Some(NOW - 60_000);
        assert_eq!(check(&s, NOW, &Limits::default()), None);
        s.oldest_approval_ms = Some(NOW - 12 * 60_000);
        assert_eq!(check(&s, NOW, &Limits::default()).unwrap().code, Code::ApprovalWaiting);
        let mut q = base(Kind::Workflow, "waiting");
        q.question = true;
        q.waiting_since_ms = Some(NOW - 45 * 60_000);
        assert_eq!(check(&q, NOW, &Limits::default()).unwrap().code, Code::QuestionWaiting);
        q.waiting_since_ms = Some(NOW - 5 * 60_000);
        assert_eq!(check(&q, NOW, &Limits::default()), None);
    }
}
