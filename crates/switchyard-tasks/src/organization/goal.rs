//! The goal check (ADR 0046, stage 2): an independent, read-only referee
//! judges whether a team is finishing its objective. This module renders its
//! prompt from the organization's state and Core's evidence, and parses its
//! verdict. The verdict only steers: Core tells the lead or asks the user;
//! nothing is approved or completed from it.

use serde::{Deserialize, Serialize};

use super::{OrgState, ROOT, TaskState};

pub const FENCE: &str = "```switchyard-goal";
const MAX_BLOCK: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every criterion is met: the team should wrap up.
    Done,
    /// The current work serves an unmet criterion.
    OnTrack,
    /// Work that does not serve the objective.
    Drifting,
    /// The same thing again and again without progress.
    Looping,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::OnTrack => "on_track",
            Self::Drifting => "drifting",
            Self::Looping => "looping",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Met,
    Partial,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionCheck {
    pub criterion: String,
    pub status: CriterionStatus,
    #[serde(default)]
    pub evidence: String,
}

/// The referee's answer, as stored with the organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalReport {
    pub verdict: Verdict,
    pub summary: String,
    #[serde(default)]
    pub criteria: Vec<CriterionCheck>,
    /// Task keys the referee judged off the objective.
    #[serde(default)]
    pub off_goal: Vec<String>,
    #[serde(default)]
    pub at_ms: i64,
    /// Which agent judged (provider and model), for the record.
    #[serde(default)]
    pub by: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    verdict: Verdict,
    summary: String,
    #[serde(default)]
    criteria: Vec<CriterionCheck>,
    #[serde(default)]
    off_goal: Vec<String>,
}

/// The JSON of the last `switchyard-goal` block. A model that misspells the
/// tag (seen: `switchyard-go`) still wrote the answer: any block tagged
/// `switchyard…` is taken, and its JSON must still pass every check below.
fn last_block(text: &str) -> Result<&str, String> {
    let start = text
        .rfind(FENCE)
        .or_else(|| text.rfind("```switchyard"))
        .ok_or("the answer has no switchyard-goal block")?;
    let after = &text[start + 3..];
    let body = &after[after.find('\n').ok_or("the block is not terminated")? + 1..];
    Ok(body[..body.find("```").ok_or("the block is not terminated")?].trim())
}

/// Parses the last `switchyard-goal` block of the referee's turn.
pub fn parse_verdict(text: &str, state: &OrgState) -> Result<GoalReport, String> {
    let json = last_block(text)?;
    if json.len() > MAX_BLOCK {
        return Err("the block is too large".into());
    }
    let b: Block = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if b.summary.trim().is_empty() {
        return Err("the summary is empty".into());
    }
    if let Some(k) = b.off_goal.iter().find(|k| state.task(k).is_none()) {
        return Err(format!("off_goal names {k}, which is not a task of the team"));
    }
    if b.verdict == Verdict::Done && b.criteria.iter().any(|c| c.status != CriterionStatus::Met) {
        return Err("\"done\" with a criterion that is not met".into());
    }
    Ok(GoalReport {
        verdict: b.verdict,
        summary: b.summary.trim().chars().take(2000).collect(),
        criteria: b.criteria.into_iter().take(30).collect(),
        off_goal: b.off_goal,
        at_ms: 0,
        by: String::new(),
    })
}

/// The referee's prompt: the objective, the work, and Core's evidence.
pub fn render_prompt(state: &OrgState, changes: &str, decisions: &[String]) -> String {
    let root = state.task(ROOT);
    let mut p = String::from(
        "SWITCHYARD GOAL CHECK\n\
         You are an independent referee for a team of coding agents. You do not work on the task and you change \
         nothing (this session is read-only). Your only job: judge whether the team is finishing its OBJECTIVE, and \
         nothing else. Be strict and concrete.\n",
    );
    if let Some(r) = root {
        p.push_str(&format!("\n## The objective\n{}\n", r.title));
        if let Some(d) = &r.description {
            p.push_str(&format!("{d}\n"));
        }
        if r.acceptance.is_empty() {
            p.push_str(
                "\nIt has no explicit done-when criteria: derive at most five concrete, checkable criteria from the \
                 objective itself, and judge against those only.\n",
            );
        } else {
            p.push_str("\n## Done when (the user's criteria)\n");
            for c in &r.acceptance {
                p.push_str(&format!("- {c}\n"));
            }
        }
    }
    p.push_str("\n## The team's tasks\n");
    for t in state.tasks.iter().filter(|t| t.key != ROOT) {
        let tag = match t.state {
            TaskState::Proposed => " — proposed by the lead outside the agreed plan, not approved".to_owned(),
            _ if t.late => " — a later area".to_owned(),
            _ => String::new(),
        };
        p.push_str(&format!(
            "- {} (key {}, {}, {}, {} turns){tag}{}\n",
            t.title,
            t.key,
            t.work.as_str(),
            t.state.as_str(),
            t.turns,
            t.summary
                .as_deref()
                .map(|s| format!(": {}", s.chars().take(300).collect::<String>()))
                .unwrap_or_default()
        ));
    }
    p.push_str(&format!(
        "\nSupervision turns since work was last accepted: {}.\n",
        state.turns_since_progress
    ));
    if !decisions.is_empty() {
        p.push_str("\n## Recent decisions (claims by the agents)\n");
        for d in decisions {
            p.push_str(&format!("- {d}\n"));
        }
    }
    p.push_str(&format!("\n## The actual changes in the code (from Git, by Switchyard)\n{changes}\n"));
    p.push_str(
        "\n## How to judge\n\
         - A criterion is met only if the changes above show it (not because an agent says so).\n\
         - done: every criterion is met; the team should wrap up now.\n\
         - on_track: the work in progress directly serves an unmet criterion.\n\
         - drifting: work in progress or planned does not serve any criterion (name those task keys in off_goal).\n\
         - looping: the same work is repeated without progress (many turns, nothing accepted).\n\
         Prefer the verdict that protects the objective. Write a short plain-language summary for the user.\n\
         End with exactly one block:\n\
         ```switchyard-goal\n\
         {\"verdict\": \"done|on_track|drifting|looping\", \"summary\": \"…\",\n \
          \"criteria\": [{\"criterion\": \"…\", \"status\": \"met|partial|open\", \"evidence\": \"file or change that shows it\"}],\n \
          \"off_goal\": [\"task-key\"]}\n\
         ```\n",
    );
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organization::tests_support::state_with_tasks;

    #[test]
    fn verdicts_are_parsed_and_checked_against_the_team() {
        let s = state_with_tasks(&["mailboxes", "derived-views"]);
        let ok = "x\n```switchyard-goal\n{\"verdict\": \"drifting\", \"summary\": \"Mailboxes are done; derived views are extra.\", \"criteria\": [{\"criterion\": \"Each user connects their own mailbox\", \"status\": \"met\", \"evidence\": \"src/lib/connectors/mail-settings.ts\"}], \"off_goal\": [\"derived-views\"]}\n```";
        let r = parse_verdict(ok, &s).unwrap();
        assert_eq!((r.verdict, r.off_goal.clone()), (Verdict::Drifting, vec!["derived-views".to_owned()]));
        assert_eq!(r.criteria[0].status, CriterionStatus::Met);
        // A misspelled tag (a real referee wrote `switchyard-go`) is still read.
        let typo = "x\n```switchyard-go\n{\"verdict\": \"on_track\", \"summary\": \"Covers it.\"}\n```";
        assert_eq!(parse_verdict(typo, &s).unwrap().verdict, Verdict::OnTrack);
        for bad in [
            "```switchyard-go\n{\"verdict\": \"on_track\", \"summary\": \"x\", \"merge\": true}\n```",
            "no block",
            "```switchyard-goal\n{\"verdict\": \"done\", \"summary\": \"x\", \"criteria\": [{\"criterion\": \"c\", \"status\": \"open\"}]}\n```",
            "```switchyard-goal\n{\"verdict\": \"drifting\", \"summary\": \"x\", \"off_goal\": [\"nope\"]}\n```",
            "```switchyard-goal\n{\"verdict\": \"approve\", \"summary\": \"x\"}\n```",
            "```switchyard-goal\n{\"verdict\": \"on_track\", \"summary\": \"x\", \"merge\": true}\n```",
        ] {
            assert!(parse_verdict(bad, &s).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_prompt_carries_the_objective_the_work_and_the_evidence() {
        let s = state_with_tasks(&["mailboxes"]);
        let p = render_prompt(&s, "2 files changed", &["Lead accepted mailboxes".into()]);
        for want in [
            "SWITCHYARD GOAL CHECK",
            "change nothing",
            "mailboxes",
            "2 files changed",
            "switchyard-goal",
            "met only if the changes",
        ] {
            assert!(p.contains(want), "{want}");
        }
    }
}
