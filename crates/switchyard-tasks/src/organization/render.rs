//! What agents are told: a supervisor's checkpoint prompt, and the
//! organization brief added to a specialist's task context.
//!
//! Each agent gets the context of its responsibility, not every
//! conversation: a supervisor sees its scope (its agents, tasks, the reports
//! and Core's verified evidence for them) and the shared decisions; a
//! specialist sees the objective, its task, its supervisor's instructions and
//! the decisions and facts that concern it. Every entry carries its status
//! (verified, claim, proposal, decided) and its source.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Checkpoint, CheckpointItem, ContextKind, ContextStatus, OrgState, OrgTask, ROOT, WorkerOutcome};
use crate::organization::directive::MAX_DIRECTIVE_BYTES;

/// A stored entry of the shared context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    /// Version of the shared context: strictly increasing per organization.
    pub seq: i64,
    pub kind: ContextKind,
    pub status: ContextStatus,
    pub text: String,
    pub task: Option<String>,
    pub source: String,
    pub supersedes: Option<i64>,
}

/// Entries still in force, with superseded ones marked.
pub fn current(entries: &[ContextEntry]) -> Vec<ContextEntry> {
    let replaced: BTreeSet<i64> = entries.iter().filter_map(|e| e.supersedes).collect();
    entries
        .iter()
        .map(|e| {
            let mut e = e.clone();
            if replaced.contains(&e.seq) {
                e.status = ContextStatus::Superseded;
            }
            e
        })
        .collect()
}

fn line(e: &ContextEntry) -> String {
    let task = e.task.as_deref().map(|t| format!(" [{t}]")).unwrap_or_default();
    format!("#{} {} ({}, from {}){task}: {}", e.seq, e.kind.as_str(), e.status.as_str(), e.source, e.text)
}

/// The tasks under `root` (inclusive), depth first.
fn subtree<'a>(state: &'a OrgState, root: &str) -> Vec<&'a OrgTask> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(k) = stack.pop() {
        if let Some(t) = state.task(&k) {
            out.push(t);
            let kids: Vec<String> = state.children(&k).map(|c| c.key.clone()).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

fn ancestors(state: &OrgState, task: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = state.task(task).and_then(|t| t.parent.clone());
    while let Some(k) = cur {
        cur = state.task(&k).and_then(|t| t.parent.clone());
        out.push(k);
        if out.len() > state.tasks.len() {
            break;
        }
    }
    out
}

/// The marker line fakes and tests use to find a supervision turn's task.
pub fn supervisor_marker(task_id: &str, agent: &str) -> String {
    let j = json!({ "version": 1, "role": "supervise", "task_id": task_id, "agent": agent });
    format!(
        "SWITCHYARD TASK CONTEXT (organization)\n```json\n{}\n```",
        serde_json::to_string_pretty(&j).unwrap_or_default()
    )
}

fn outcome_text(o: &WorkerOutcome) -> String {
    match o {
        WorkerOutcome::Delivered { summary } => format!("reported its work: {summary}"),
        WorkerOutcome::Reviewed { result, summary } => format!("reviewed ({}): {summary}", result.as_str()),
        WorkerOutcome::Challenge { summary, details } => format!(
            "challenges its instructions: {summary}{}",
            details.as_deref().map(|d| format!(" — evidence: {d}")).unwrap_or_default()
        ),
        WorkerOutcome::Blocker { summary } => format!("is blocked: {summary}"),
        WorkerOutcome::Question { summary } => format!("asks: {summary}"),
        WorkerOutcome::Failed { reason } => format!("ended without a usable result: {reason}"),
        WorkerOutcome::Interrupted { reason } => format!("was interrupted: {reason}"),
    }
}

/// The prompt of one supervision turn. `available` lists the providers and
/// models Core can run, with their capability facts (one line each).
pub fn render_supervisor(
    state: &OrgState,
    checkpoint: &Checkpoint,
    entries: &[ContextEntry],
    available: &[String],
) -> String {
    let me = state.agent(&checkpoint.supervisor);
    let task = state.task(&checkpoint.task);
    let root = state.task(ROOT);
    let mut p = String::new();
    p.push_str(&supervisor_marker(
        &task.and_then(|t| t.task_id).map(|i| i.to_string()).unwrap_or_default(),
        &checkpoint.supervisor,
    ));
    p.push_str("\n\nYou are a supervisor in an engineering organization coordinated by Switchyard.\n");
    if let Some(a) = me {
        p.push_str(&format!(
            "You are \"{}\" (key {}), responsible for: {}\nYou report to: {}.\n",
            a.title,
            a.key,
            a.responsibility,
            a.reports_to
                .as_deref()
                .and_then(|r| state.agent(r))
                .map_or_else(|| "the user".to_owned(), |b| format!("{} (key {})", b.title, b.key)),
        ));
    }
    if let Some(t) = task {
        p.push_str(&format!("\n## Your scope: {} (task key {})\n", t.title, t.key));
        if let Some(d) = &t.description {
            p.push_str(&format!("{d}\n"));
        }
        for c in &t.acceptance {
            p.push_str(&format!("- done when: {c}\n"));
        }
    }
    if let Some(r) = root
        && checkpoint.task != ROOT
    {
        p.push_str(&format!("\n## The overall objective\n{}\n", r.title));
        for c in &r.acceptance {
            p.push_str(&format!("- done when: {c}\n"));
        }
    }
    p.push_str("\n## Your organization (you and the agents reporting to you)\n");
    for a in state.agents.iter().filter(|a| state.in_subtree(&a.key, &checkpoint.supervisor)) {
        p.push_str(&format!(
            "- {} (key {}, {}, reports to {}): {}; scope {}; {} {}; {}\n",
            a.title,
            a.key,
            a.kind.as_str(),
            a.reports_to.as_deref().unwrap_or("the user"),
            a.responsibility,
            if a.scope.is_empty() { "the whole workspace".to_owned() } else { a.scope.join(", ") },
            a.provider,
            a.model.as_deref().unwrap_or("(CLI default model)"),
            a.status.as_str(),
        ));
    }
    p.push_str("\n## Tasks in your scope\n");
    for t in subtree(state, &checkpoint.task) {
        p.push_str(&format!(
            "- {} (key {}, {}, owner {}, {}{}{}){}\n",
            t.title,
            t.key,
            t.work.as_str(),
            t.owner,
            t.state.as_str(),
            if t.depends_on.is_empty() {
                String::new()
            } else {
                format!(", after {}", t.depends_on.join(", "))
            },
            if t.corrections > 0 { format!(", {} corrections", t.corrections) } else { String::new() },
            t.summary.as_deref().map(|s| format!(": {s}")).unwrap_or_default(),
        ));
    }
    p.push_str("\n## What needs your attention now\n");
    for item in &checkpoint.items {
        let text = match item {
            CheckpointItem::Plan => {
                "Break your scope down: decide which agents you need (only what the work requires; a small \
                 change may need one specialist) and give each task an owner, a scope and acceptance criteria."
                    .to_owned()
            }
            CheckpointItem::Reported { task, outcome, evidence } => {
                let who = state.task(task).map(|t| t.owner.clone()).unwrap_or_default();
                let mut s = format!("Task {task}: {who} {}", outcome_text(outcome));
                if !evidence.changed_files.is_empty() {
                    s.push_str(&format!(
                        "\n  Verified by Switchyard (Git): files changed in the workspace: {}",
                        evidence.changed_files.join(", ")
                    ));
                }
                if !evidence.outside_scope.is_empty() {
                    s.push_str(&format!(
                        "\n  Verified by Switchyard: OUTSIDE the task's scope: {}",
                        evidence.outside_scope.join(", ")
                    ));
                }
                s.push_str(
                    "\n  Evaluate it against its acceptance criteria and the evidence (read the files), not \
                     the report alone, then accept, correct, reassign or cancel it.",
                );
                s
            }
            CheckpointItem::Observed { task, text } => {
                format!("Task {task}: Switchyard observed that {text}, and interrupted that turn. Decide how to continue.")
            }
            CheckpointItem::Escalated { from, question } => format!("{from} escalates to you: {question}"),
            CheckpointItem::Answer { text } => format!("The user answered your escalation: {text}"),
            CheckpointItem::Rejected { error } => {
                format!("Your previous directive was rejected and changed nothing: {error}. Send a corrected one.")
            }
            CheckpointItem::Instruction { from, text } => {
                format!("{from} sent your finished scope back: {text}")
            }
            CheckpointItem::ChildrenFinished => "Every task in your scope is accepted or cancelled. Check the \
                 result against your acceptance criteria; add tasks if something is missing, or report \
                 \"done\" with a summary that names any outstanding issues."
                .to_owned(),
        };
        p.push_str(&format!("- {text}\n"));
    }
    let relevant: BTreeSet<String> = subtree(state, &checkpoint.task).iter().map(|t| t.key.clone()).collect();
    let anc = ancestors(state, &checkpoint.task);
    let shown: Vec<String> = current(entries)
        .iter()
        .filter(|e| e.status != ContextStatus::Superseded)
        .filter(|e| match &e.task {
            None => true,
            Some(t) => relevant.contains(t) || anc.contains(t),
        })
        .rev()
        .take(60)
        .map(line)
        .collect();
    p.push_str("\n## Shared engineering context (newest first; verified = checked by Switchyard, claim = stated by an agent)\n");
    for l in shown {
        p.push_str(&format!("{l}\n"));
    }
    p.push_str("\n## Providers and models Switchyard can run\n");
    for a in available {
        p.push_str(&format!("- {a}\n"));
    }
    let l = &state.limits;
    p.push_str(&format!(
        "\n## Limits Switchyard enforces\nAt most {} agents ({} now), {} tasks ({} now), {} levels of reporting, \
         {} correction rounds per task, one session at a time (agents work one after another in one workspace). \
         A directive that exceeds a limit is rejected and changes nothing; escalate to the user if you need more.\n",
        l.max_agents,
        state.agents.len(),
        l.max_tasks,
        state.tasks.len(),
        l.max_depth,
        l.max_corrections,
    ));
    p.push_str(&format!(
        "\n## How to answer\nYou may read the workspace to check the work; do not change files (this session \
         is read-only) and do not start sub-agents yourself. End your final message with exactly one block \
         (at most {MAX_DIRECTIVE_BYTES} bytes of JSON); keep the message short:\n\
```switchyard-org\n\
{{\"summary\": \"what you decided and why\",\n\
 \"agents\": [{{\"key\": \"docs\", \"title\": \"Documentation specialist\", \"kind\": \"specialist\", \"reports_to\": \"{me}\",\n\
   \"responsibility\": \"...\", \"scope\": [\"docs/\"], \"capabilities\": [\"documentation\"], \"provider\": \"optional\", \"model\": \"optional\"}}],\n\
 \"tasks\": [{{\"key\": \"write-docs\", \"title\": \"...\", \"description\": \"...\", \"work\": \"documentation\", \"owner\": \"docs\",\n\
   \"scope\": [\"docs/\"], \"acceptance\": [\"...\"], \"depends_on\": [\"other-task-key\"]}}],\n\
 \"assessments\": [{{\"task\": \"key\", \"verdict\": \"accept|correct|reassign|cancel\", \"instructions\": \"...\", \"to\": \"agent key\", \"evidence\": \"...\"}}],\n\
 \"context\": [{{\"kind\": \"decision|finding|risk|progress\", \"text\": \"...\", \"status\": \"claim|proposal|decided\", \"supersedes\": 12}}],\n\
 \"escalate\": {{\"to\": \"supervisor|user\", \"question\": \"...\"}},\n\
 \"done\": false}}\n\
```\n\
Work kinds: implementation, investigation, documentation, testing, review (owned by a specialist), and \
coordination (owned by a supervisor who reports to you; it breaks that part down itself). Omit what you do not \
need. Resolve ordinary engineering decisions yourself; escalate only what is consequential or unresolved. \
The directive does not grant permissions: every tool request still goes through Switchyard's policies, and \
only the user approves delivery.",
        me = checkpoint.supervisor,
    ));
    p
}

/// The organization part of a specialist's task context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgBrief {
    pub objective: String,
    pub your_role: String,
    pub responsibility: String,
    /// Workspace-relative paths this task may change (empty: no file changes expected).
    pub scope: Vec<String>,
    pub supervisor: String,
    /// The supervisor's instructions for this turn (decided).
    #[serde(default)]
    pub instructions: Vec<String>,
    /// Decisions, facts and risks that concern this task, each labelled.
    #[serde(default)]
    pub shared_context: Vec<String>,
}

/// The brief for the owner of `task`, for the turn that carries `instructions`.
pub fn worker_brief(
    state: &OrgState,
    task: &str,
    entries: &[ContextEntry],
    instructions: &[String],
) -> Option<OrgBrief> {
    let t = state.task(task)?;
    let owner = state.agent(&t.owner)?;
    let supervisor = state
        .reviewer_of(task)
        .and_then(|k| {
            state.agent(&k).map(|a| {
                format!("{} (key {}, {} {})", a.title, a.key, a.provider, a.model.as_deref().unwrap_or(""))
            })
        })
        .unwrap_or_else(|| "the user".into());
    let root = state.task(ROOT)?;
    let mut objective = root.title.clone();
    if !root.acceptance.is_empty() {
        objective.push_str(&format!(" (done when: {})", root.acceptance.join("; ")));
    }
    let anc = ancestors(state, task);
    let shared: Vec<String> = current(entries)
        .iter()
        .filter(|e| e.status != ContextStatus::Superseded)
        .filter(|e| {
            matches!(
                e.kind,
                ContextKind::Decision
                    | ContextKind::Finding
                    | ContextKind::Risk
                    | ContextKind::Instruction
                    | ContextKind::Observation
            )
        })
        .filter(|e| match &e.task {
            None => true,
            Some(k) => k == task || anc.contains(k),
        })
        .filter(|e| !(e.kind == ContextKind::Instruction && e.task.as_deref() == Some(task)))
        .rev()
        .take(30)
        .map(line)
        .collect();
    Some(OrgBrief {
        objective,
        your_role: format!("{} (key {})", owner.title, owner.key),
        responsibility: owner.responsibility.clone(),
        scope: if t.work.changes_files() { t.scope.clone() } else { Vec::new() },
        supervisor,
        instructions: instructions.to_vec(),
        shared_context: shared,
    })
}
