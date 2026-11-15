use std::collections::BTreeMap;

use super::*;

const T0: i64 = 1_000_000;

fn lead() -> Agent {
    Agent {
        id: Uuid::now_v7(),
        key: LEAD.into(),
        title: "Engineering lead".into(),
        kind: AgentKind::Supervisor,
        reports_to: None,
        responsibility: "Deliver the objective".into(),
        scope: vec![],
        capabilities: vec!["reasoning".into()],
        provider: "codex".into(),
        model: None,
        selection: "chosen by the user".into(),
        status: AgentStatus::Idle,
        created_by: "user".into(),
        depth: 1,
    }
}

fn objective() -> OrgTask {
    OrgTask {
        key: ROOT.into(),
        task_id: Some(Uuid::now_v7()),
        title: "Greet users in their language".into(),
        description: Some("Add a greeting".into()),
        work: WorkKind::Coordination,
        parent: None,
        owner: LEAD.into(),
        scope: vec![],
        acceptance: vec!["Greets in Estonian".into()],
        depends_on: vec![],
        state: TaskState::Planning,
        corrections: 0,
        attempts: 0,
        instructions: vec![],
        current_instructions: vec![],
        summary: None,
        created_by: "user".into(),
    }
}

fn empty() -> OrgState {
    OrgState {
        status: OrgStatus::Running,
        reason: None,
        limits: Limits::default(),
        agents: vec![],
        tasks: vec![],
        queue: vec![],
        activity: Activity::Idle,
        repairs: 0,
        supervision_turns: 0,
        started_at_ms: 0,
        cost_usd: 0.0,
        tokens: 0,
        question_from: None,
        final_summary: None,
        sessions: BTreeMap::new(),
        turn_after_seq: 0,
    }
}

fn started() -> OrgState {
    let d = decide(
        &empty(),
        &Input::Start { objective: Box::new(objective()), lead: Box::new(lead()), now_ms: T0 },
    )
    .unwrap();
    assert!(
        matches!(&d.action, Action::Supervise { checkpoint } if checkpoint.supervisor == LEAD && checkpoint.items == vec![CheckpointItem::Plan])
    );
    assert!(
        d.context.iter().any(|c| c.kind == ContextKind::Objective && c.status == ContextStatus::Verified)
    );
    d.state
}

fn directive(json: &str) -> Directive {
    parse_directive(&format!("```switchyard-org\n{json}\n```")).unwrap()
}

/// Supervises with Core's usual answers: every requested agent gets `codex`.
fn supervise(state: &OrgState, agent: &str, json: &str) -> Decision {
    let dir = directive(json);
    let mut selections = BTreeMap::new();
    let mut ids = BTreeMap::new();
    for a in &dir.agents {
        selections.insert(
            a.key.clone(),
            Ok(Selected { provider: "claude_code".into(), model: None, rationale: "test".into() }),
        );
        ids.insert(format!("agent:{}", a.key), Uuid::now_v7());
    }
    for t in &dir.tasks {
        ids.insert(format!("task:{}", t.key), Uuid::now_v7());
    }
    decide(
        state,
        &Input::Supervised {
            agent: agent.into(),
            directive: Ok(dir),
            selections,
            new_ids: ids,
            now_ms: T0 + 1,
        },
    )
    .unwrap()
}

fn worked(state: &OrgState, task: &str, outcome: WorkerOutcome, evidence: Evidence) -> Decision {
    decide(state, &Input::Worked { task: task.into(), outcome, evidence, now_ms: T0 + 2 }).unwrap()
}

const PLAN: &str = r#"{"summary": "One implementer is enough",
  "agents": [{"key": "impl", "title": "Implementation specialist", "kind": "specialist",
    "responsibility": "Implement the greeting", "scope": ["src"], "capabilities": ["coding"]}],
  "tasks": [{"key": "greet", "title": "Implement the greeting", "work": "implementation", "owner": "impl",
    "acceptance": ["Greets in Estonian"]}]}"#;

fn delivered(s: &str) -> WorkerOutcome {
    WorkerOutcome::Delivered { summary: s.into() }
}

#[test]
fn a_plan_creates_agents_and_tasks_and_starts_the_first_work() {
    let d = supervise(&started(), LEAD, PLAN);
    assert_eq!(d.created_tasks, vec!["greet"]);
    let impl_agent = d.state.agent("impl").unwrap();
    assert_eq!((impl_agent.depth, impl_agent.reports_to.as_deref()), (2, Some(LEAD)));
    let greet = d.state.task("greet").unwrap();
    assert_eq!(greet.scope, vec!["src"], "work inherits its owner's scope");
    assert!(greet.task_id.is_some(), "Core pre-allocates the M6 task id");
    assert_eq!(d.state.task(ROOT).unwrap().state, TaskState::Delegated);
    assert_eq!(d.action, Action::Work { task: "greet".into(), agent: "impl".into(), instructions: vec![] });
    assert!(matches!(d.state.activity, Activity::Working { ref task, .. } if task == "greet"));
    assert_eq!(d.state.supervision_turns, 1);
}

#[test]
fn the_supervisor_reviews_verified_evidence_and_corrects_scope_drift() {
    let s = supervise(&started(), LEAD, PLAN).state;
    let evidence = Evidence {
        changed_files: vec!["src/greet.txt".into(), "scratch.txt".into()],
        outside_scope: vec!["scratch.txt".into()],
    };
    let d = worked(&s, "greet", delivered("Added the greeting"), evidence);
    // The verified observation goes into the shared context and the checkpoint.
    assert!(d.context.iter().any(|c| c.kind == ContextKind::Observation
        && c.status == ContextStatus::Verified
        && c.text.contains("scratch.txt")));
    let Action::Supervise { checkpoint } = &d.action else { panic!("{:?}", d.action) };
    assert_eq!(checkpoint.supervisor, LEAD);
    assert!(
        matches!(&checkpoint.items[0], CheckpointItem::Reported { evidence, .. } if evidence.outside_scope == vec!["scratch.txt"])
    );
    // Correction goes back to the same specialist with the instruction.
    let d = supervise(
        &d.state,
        LEAD,
        r#"{"summary": "Out of scope", "assessments": [{"task": "greet", "verdict": "correct",
            "instructions": "Delete scratch.txt; change only src/", "evidence": "Switchyard saw scratch.txt"}]}"#,
    );
    assert_eq!(
        d.action,
        Action::Work {
            task: "greet".into(),
            agent: "impl".into(),
            instructions: vec!["Delete scratch.txt; change only src/".into()]
        }
    );
    assert_eq!(d.state.task("greet").unwrap().corrections, 1);
    assert!(
        d.context.iter().any(|c| c.kind == ContextKind::Instruction && c.status == ContextStatus::Decided)
    );
    // The corrected work is accepted; then the lead sees everything finished and reports done.
    let d = worked(&d.state, "greet", delivered("Removed scratch.txt"), Evidence::default());
    let d = supervise(
        &d.state,
        LEAD,
        r#"{"summary": "Good", "assessments": [{"task": "greet", "verdict": "accept"}]}"#,
    );
    assert_eq!(d.closed_tasks, vec![("greet".to_owned(), true)], "{:?}", d.changes);
    let Action::Supervise { checkpoint } = &d.action else { panic!("{:?}", d.action) };
    assert_eq!(checkpoint.items, vec![CheckpointItem::ChildrenFinished]);
    let d = supervise(&d.state, LEAD, r#"{"summary": "Done. Outstanding: none.", "done": true}"#);
    assert_eq!(d.action, Action::Deliver { summary: "Done. Outstanding: none.".into() });
    assert_eq!(d.state.status, OrgStatus::Done);
    // Delivery can send it back to the lead.
    let back = decide(&d.state, &Input::Reopen { text: "validation failed".into(), now_ms: T0 + 9 }).unwrap();
    let Action::Supervise { checkpoint } = &back.action else { panic!("{:?}", back.action) };
    assert!(
        matches!(&checkpoint.items[0], CheckpointItem::Instruction { text, .. } if text == "validation failed")
    );
    // Inputs after the end change nothing.
    let after = worked(&d.state, "greet", delivered("late"), Evidence::default());
    assert_eq!(after.state, d.state);
}

#[test]
fn done_is_refused_while_work_is_open_and_the_directive_changes_nothing() {
    let s = supervise(&started(), LEAD, PLAN).state;
    let s = worked(&s, "greet", delivered("x"), Evidence::default()).state;
    let d = supervise(&s, LEAD, r#"{"summary": "All done", "done": true}"#);
    assert!(matches!(&d.changes[0], Change::DirectiveRejected { error, .. } if error.contains("still open")));
    let Action::Supervise { checkpoint } = &d.action else { panic!() };
    assert!(checkpoint.items.iter().any(|i| matches!(i, CheckpointItem::Rejected { .. })));
    assert_eq!(d.state.task("greet").unwrap().state, TaskState::Reported);
}

#[test]
fn limits_and_authority_are_enforced() {
    let s = started();
    let mut few = s.clone();
    few.limits.max_agents = 1;
    for (state, json, expect) in [
        (&few, PLAN, "its limit"),
        (
            &s,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "A", "kind": "specialist", "responsibility": "r", "reports_to": "nobody"}]}"#,
            "not in the organization",
        ),
        (
            &s,
            r#"{"summary": "x", "tasks": [{"key": "t", "title": "T", "work": "implementation", "owner": "lead", "scope": ["src"]}]}"#,
            "specialist",
        ),
        (
            &s,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "A", "kind": "specialist", "responsibility": "r"}],
              "tasks": [{"key": "t", "title": "T", "work": "implementation", "owner": "a"}]}"#,
            "needs a scope",
        ),
        (
            &s,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "A", "kind": "specialist", "responsibility": "r", "scope": ["src"]}],
              "tasks": [{"key": "t", "title": "T", "work": "implementation", "owner": "a"},
                        {"key": "u", "title": " t ", "work": "implementation", "owner": "a"}]}"#,
            "already exists",
        ),
        (
            &s,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "A", "kind": "specialist", "responsibility": "r", "scope": ["src"]}],
              "tasks": [{"key": "t", "title": "T", "work": "implementation", "owner": "a", "depends_on": ["zz"]}]}"#,
            "does not exist",
        ),
        (
            &s,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "A", "kind": "specialist", "responsibility": "r", "scope": ["src"]},
                                          {"key": "b", "title": "B", "kind": "specialist", "responsibility": "r", "reports_to": "a"}]}"#,
            "not a supervisor",
        ),
        (&s, r#"{"summary": "Nothing"}"#, "a plan needs at least one task"),
    ] {
        let d = supervise(state, LEAD, json);
        let err = d.changes.iter().find_map(|c| match c {
            Change::DirectiveRejected { error, .. } => Some(error.clone()),
            _ => None,
        });
        assert!(err.as_deref().is_some_and(|e| e.contains(expect)), "{json}: {err:?}");
        assert!(
            d.state.agents.len() == state.agents.len() && d.state.tasks.len() == state.tasks.len(),
            "atomic: {json}"
        );
    }
    // Depth.
    let mut shallow = s.clone();
    shallow.limits.max_depth = 2;
    let d = supervise(
        &shallow,
        LEAD,
        r#"{"summary": "x", "agents": [{"key": "sub", "title": "Sub", "kind": "supervisor", "responsibility": "r"},
            {"key": "deep", "title": "Deep", "kind": "specialist", "responsibility": "r", "reports_to": "sub", "scope": ["a"]}]}"#,
    );
    assert!(
        d.changes
            .iter()
            .any(|c| matches!(c, Change::DirectiveRejected { error, .. } if error.contains("levels")))
    );
}

#[test]
fn corrections_are_bounded() {
    let mut s = supervise(&started(), LEAD, PLAN).state;
    s.limits.max_corrections = 1;
    let s = worked(&s, "greet", delivered("x"), Evidence::default()).state;
    let correct = r#"{"summary": "again", "assessments": [{"task": "greet", "verdict": "correct", "instructions": "fix it"}]}"#;
    let s = supervise(&s, LEAD, correct).state;
    let s = worked(&s, "greet", delivered("y"), Evidence::default()).state;
    let d = supervise(&s, LEAD, correct);
    assert!(d.changes.iter().any(
        |c| matches!(c, Change::DirectiveRejected { error, .. } if error.contains("correction rounds"))
    ));
}

#[test]
fn invalid_directives_are_retried_then_a_person_decides() {
    let mut s = started();
    for n in 1..=MAX_REPAIRS + 1 {
        let d = decide(
            &s,
            &Input::Supervised {
                agent: LEAD.into(),
                directive: Err("the turn ended without a switchyard-org block".into()),
                selections: BTreeMap::new(),
                new_ids: BTreeMap::new(),
                now_ms: T0,
            },
        )
        .unwrap();
        s = d.state;
        if n <= MAX_REPAIRS {
            assert!(matches!(d.action, Action::Supervise { .. }), "repair turn {n}");
        } else {
            assert_eq!(s.status, OrgStatus::Waiting);
            assert!(s.reason.as_deref().unwrap().contains("rejected"));
        }
    }
    // Resume asks the same supervisor again, with the error.
    let d = decide(&s, &Input::Resume { now_ms: T0 }).unwrap();
    let Action::Supervise { checkpoint } = d.action else { panic!() };
    assert!(checkpoint.items.iter().any(|i| matches!(i, CheckpointItem::Rejected { .. })));
}

#[test]
fn several_supervisors_report_through_the_hierarchy() {
    let s = supervise(
        &started(),
        LEAD,
        r#"{"summary": "Code and docs",
          "agents": [{"key": "impl", "title": "Implementer", "kind": "specialist", "responsibility": "code", "scope": ["src"]},
                     {"key": "docs-lead", "title": "Documentation supervisor", "kind": "supervisor", "responsibility": "docs", "scope": ["docs"]}],
          "tasks": [{"key": "code", "title": "Code", "work": "implementation", "owner": "impl"},
                    {"key": "docs", "title": "Document it", "work": "coordination", "owner": "docs-lead", "depends_on": ["code"]}]}"#,
    )
    .state;
    assert!(matches!(&d_action(&s), Some(t) if t == "code"));
    let s = worked(&s, "code", delivered("done"), Evidence::default()).state;
    let d =
        supervise(&s, LEAD, r#"{"summary": "ok", "assessments": [{"task": "code", "verdict": "accept"}]}"#);
    // The documentation supervisor now plans its own scope.
    let Action::Supervise { checkpoint } = &d.action else { panic!("{:?}", d.action) };
    assert_eq!((checkpoint.supervisor.as_str(), checkpoint.task.as_str()), ("docs-lead", "docs"));
    let d = supervise(
        &d.state,
        "docs-lead",
        r#"{"summary": "One writer", "agents": [{"key": "writer", "title": "API docs writer", "kind": "specialist", "responsibility": "write", "scope": ["docs"]}],
           "tasks": [{"key": "api-docs", "title": "API docs", "work": "documentation", "owner": "writer"}]}"#,
    );
    assert_eq!(d.state.agent("writer").unwrap().depth, 3);
    assert_eq!(d.state.reviewer_of("api-docs").as_deref(), Some("docs-lead"));
    // The lead cannot assess work in the docs supervisor's scope.
    let s = worked(&d.state, "api-docs", delivered("written"), Evidence::default()).state;
    let Activity::Supervising { checkpoint } = &s.activity else { panic!() };
    assert_eq!(checkpoint.supervisor, "docs-lead");
    let s = supervise(
        &s,
        "docs-lead",
        r#"{"summary": "fine", "assessments": [{"task": "api-docs", "verdict": "accept"}]}"#,
    )
    .state;
    // All its work is finished: it reports done; the lead reviews the coordination task.
    let d = supervise(&s, "docs-lead", r#"{"summary": "Docs complete", "done": true}"#);
    let Action::Supervise { checkpoint } = &d.action else { panic!("{:?}", d.action) };
    assert_eq!(checkpoint.supervisor, LEAD);
    assert!(matches!(&checkpoint.items[0], CheckpointItem::Reported { task, .. } if task == "docs"));
    let d = supervise(
        &d.state,
        LEAD,
        r#"{"summary": "ok", "assessments": [{"task": "docs", "verdict": "accept"}]}"#,
    );
    let d = supervise(&d.state, LEAD, r#"{"summary": "All done", "done": true}"#);
    assert!(matches!(d.action, Action::Deliver { .. }));
}

fn d_action(s: &OrgState) -> Option<String> {
    match &s.activity {
        Activity::Working { task, .. } => Some(task.clone()),
        _ => None,
    }
}

#[test]
fn an_observation_interrupts_the_turn_once_and_the_supervisor_decides() {
    let s = supervise(&started(), LEAD, PLAN).state;
    let obs = Input::Observed {
        task: "greet".into(),
        observation: Observation::OutsideScope { paths: vec!["README.md".into()] },
    };
    let d = decide(&s, &obs).unwrap();
    assert_eq!(d.action, Action::Interrupt { task: "greet".into() });
    assert!(d.context.iter().any(|c| c.kind == ContextKind::Observation && c.text.contains("README.md")));
    assert_eq!(decide(&d.state, &obs).unwrap().action, Action::None, "interrupted once");
    let d = worked(
        &d.state,
        "greet",
        WorkerOutcome::Interrupted { reason: "interrupted by Switchyard".into() },
        Evidence::default(),
    );
    let Action::Supervise { checkpoint } = &d.action else { panic!("{:?}", d.action) };
    assert!(checkpoint.items.iter().any(|i| matches!(i, CheckpointItem::Observed { .. })));
    assert!(checkpoint.items.iter().any(|i| matches!(i, CheckpointItem::Reported { .. })));
}

#[test]
fn failed_turns_are_retried_once_then_reviewed() {
    let s = supervise(&started(), LEAD, PLAN).state;
    let d = worked(&s, "greet", WorkerOutcome::Failed { reason: "no report".into() }, Evidence::default());
    let Action::Work { instructions, .. } = &d.action else { panic!("{:?}", d.action) };
    assert!(instructions[0].contains("no report"));
    let d = worked(&d.state, "greet", WorkerOutcome::Failed { reason: "again".into() }, Evidence::default());
    assert!(matches!(d.action, Action::Supervise { .. }));
}

#[test]
fn escalations_reach_the_user_and_the_answer_returns_to_the_supervisor() {
    let d = supervise(
        &started(),
        LEAD,
        r#"{"summary": "Unclear", "escalate": {"to": "user", "question": "Which languages?"}}"#,
    );
    assert_eq!(d.state.status, OrgStatus::Waiting);
    assert_eq!(d.state.question_from.as_deref(), Some(LEAD));
    assert!(decide(&d.state, &Input::Resume { now_ms: T0 }).is_err(), "an escalation needs an answer");
    let d = decide(&d.state, &Input::Answer { text: "Estonian and English".into(), now_ms: T0 }).unwrap();
    let Action::Supervise { checkpoint } = d.action else { panic!() };
    assert_eq!(checkpoint.items[0], CheckpointItem::Answer { text: "Estonian and English".into() });
}

#[test]
fn stale_inputs_cancel_pause_and_limits() {
    let s = supervise(&started(), LEAD, PLAN).state;
    assert_eq!(worked(&s, "other", delivered("x"), Evidence::default()).state, s, "not the running task");
    let d = decide(&s, &Input::Pause).unwrap();
    let paused = worked(&d.state, "greet", delivered("x"), Evidence::default());
    assert_eq!(paused.action, Action::None, "nothing new starts while paused");
    assert_eq!(paused.state.queue.len(), 1, "the checkpoint waits");
    let d = decide(&paused.state, &Input::Resume { now_ms: T0 + 5 }).unwrap();
    assert!(matches!(d.action, Action::Supervise { .. }));
    let d = decide(&d.state, &Input::Cancel { reason: "no longer needed".into() }).unwrap();
    assert_eq!((d.state.status, d.action.clone()), (OrgStatus::Cancelled, Action::Stop));
    // Time limit.
    let mut s = started();
    s.activity = Activity::Idle;
    s.queue.clear();
    s.tasks[0].state = TaskState::Delegated;
    let d = decide(&s, &Input::Tick { now_ms: T0 + 241 * 60_000 }).unwrap();
    assert!(d.state.reason.as_deref().unwrap().contains("time limit"));
}

#[test]
fn scope_matching() {
    let scope = vec!["src/".to_owned(), "docs/api.md".to_owned()];
    assert!(in_scope("src/a.rs", &scope) && in_scope("./src/x/y", &scope) && in_scope("docs/api.md", &scope));
    assert!(
        !in_scope("srcx/a", &scope) && !in_scope("docs/other.md", &scope) && !in_scope("README.md", &scope)
    );
    assert!(in_scope("anything", &[".".to_owned()]));
}

#[test]
fn prompts_carry_scope_evidence_and_status_labels() {
    let s = supervise(&started(), LEAD, PLAN).state;
    let d = worked(
        &s,
        "greet",
        delivered("Added"),
        Evidence { changed_files: vec!["x".into()], outside_scope: vec!["x".into()] },
    );
    let Action::Supervise { checkpoint } = &d.action else { panic!() };
    let entries: Vec<ContextEntry> = d
        .context
        .iter()
        .enumerate()
        .map(|(i, c)| ContextEntry {
            seq: i as i64 + 1,
            kind: c.kind,
            status: c.status,
            text: c.text.clone(),
            task: c.task.clone(),
            source: c.source.clone(),
            supersedes: None,
        })
        .collect();
    let p = render_supervisor(&d.state, checkpoint, &entries, &["codex: gpt-6-luna".into()]);
    assert!(p.contains("\"role\": \"supervise\"") && p.contains("OUTSIDE the task's scope: x"));
    assert!(p.contains("(verified, from core)") && p.contains("```switchyard-org"));
    assert!(p.contains("one session at a time"));
    let b = worker_brief(&d.state, "greet", &entries, &["do it".into()]).unwrap();
    assert_eq!(b.scope, vec!["src"]);
    assert!(b.supervisor.contains("Engineering lead"));
    let superseding = ContextEntry { seq: 99, supersedes: Some(1), ..entries[0].clone() };
    let all = [entries.clone(), vec![superseding]].concat();
    assert_eq!(current(&all)[0].status, ContextStatus::Superseded);
}
