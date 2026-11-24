//! The team failure matrix: every step of a team's run / every way it can go
//! wrong, each through the real Core with the fake CLIs.
//!
//! A team plans (lead), implements (specialist), is assessed (lead), reviewed
//! (reviewer), assessed again and finished (lead). At one of those steps a
//! fault is injected: the turn fails without output (a usage limit), the CLI
//! crashes, the turn ends without its report or directive, Core restarts
//! while the turn runs, or the user cancels the subtask that is being worked
//! on. The lead is adaptive (`fake_support/auto_lead.rs`): it reacts to what
//! its checkpoint shows, as a reasonable lead would.
//!
//! The user is played by a simple policy: answer a question with "start it
//! afresh", resume any other wait. A case passes when the team finishes
//! (status done, ready for delivery) with at most `MAX_INTERVENTIONS` such
//! actions. It fails when the team stalls (nothing changes while it says it
//! is running), when a resume leads back to the same wait (a dead end), when
//! it is cancelled, or when it needs more of the user than that.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::methods::{
    ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, EventsList,
    EventsListParams, OrganizationsAnswer, OrganizationsAnswerParams, OrganizationsGet, ProjectsAdd,
    ProjectsAddParams, TaskDecisionParams, TaskIdParams, TasksCancel, TasksCreate, TasksCreateParams,
    WorkflowsResume, WorkflowsStart, WorkflowsStartParams,
};
use switchyard_protocol::model::{OrganizationInfo, RoleBinding, TaskKind};
use uuid::Uuid;

use common::{TestCore, git_repo};

/// The user's actions (answers and resumes) a team may need after one fault.
const MAX_INTERVENTIONS: u32 = 3;
/// How long a running team may show no change at all before it counts as stalled.
const STALL: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// The lead's first turn.
    Plan,
    /// The specialist's first turn.
    Implement,
    /// The lead's turn on the implementation.
    AssessImplementation,
    /// The reviewer's first turn.
    Review,
    /// The lead's turn on the review.
    AssessReview,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    /// The turn fails without output (a usage limit, an API error).
    Fail,
    /// The CLI process dies mid-turn.
    Crash,
    /// The turn ends normally but without its report or directive.
    NoBlock,
    /// Core restarts while the turn waits for an approval.
    Restart,
    /// The user cancels the subtask while it is worked on.
    CancelSubtask,
    /// Every turn of the step's agent fails (a provider at its usage limit
    /// for days). The team cannot finish: it must reach the user with the
    /// cause, and resuming must not loop on the same failure.
    FailAlways,
    /// The user stops the running step (cancels its assignment, not the task).
    StopStep,
    /// The agent's process keeps running for seconds after its turn (a slow
    /// exit): the next session must not collide with it in the workspace.
    SlowExit,
    /// Core restarts while the turn runs, and again while the team waits.
    RestartTwice,
    /// Nothing goes wrong (the baseline).
    None,
}

impl Step {
    fn lead_turn(self) -> Option<usize> {
        match self {
            Self::Plan => Some(0),
            Self::AssessImplementation => Some(1),
            Self::AssessReview => Some(2),
            Self::Implement | Self::Review => None,
        }
    }
    fn subtask(self) -> Option<&'static str> {
        match self {
            Self::Implement => Some("code"),
            Self::Review => Some("review"),
            _ => None,
        }
    }
}

fn hex(v: &Value) -> String {
    v.to_string().bytes().map(|b| format!("{b:02x}")).collect()
}

fn script(spec: &Value) -> String {
    format!("[fake:report:{}]", hex(spec))
}

/// The scripted step that plays `fault`.
fn fault_step(fault: Fault) -> Value {
    match fault {
        Fault::Fail | Fault::FailAlways => json!({ "fail": "You've hit your usage limit. Try again later." }),
        Fault::Crash => json!({ "crash": true }),
        Fault::NoBlock => json!({ "text": "I looked at it and I am not sure what to do." }),
        // Shell commands ask: the turn holds on the approval while the fault happens.
        Fault::Restart | Fault::CancelSubtask | Fault::StopStep | Fault::RestartTwice => {
            json!({ "tool": "tool:Bash|{\"command\": \"echo hold\"}" })
        }
        // The slow exit is the scenario's `linger_ms`; the turn itself is good.
        Fault::SlowExit => Value::Null,
        Fault::None => Value::Null,
    }
}

fn implementation() -> Value {
    json!({ "tool": "write:src/greet.txt", "report": { "type": "implementation", "summary": "Done",
        "changed_files": ["src/greet.txt"] } })
}

fn approval() -> Value {
    json!({ "report": { "type": "review", "summary": "Correct", "result": "approved" } })
}

/// Turns of a specialist: the fault first (when it is this step's), then good turns.
fn turns(good: Value, fault: Option<Value>, always: bool) -> Value {
    if always && let Some(f) = fault {
        return Value::Array(std::iter::repeat_n(f, 12).collect());
    }
    let mut v: Vec<Value> = fault.into_iter().collect();
    v.extend(std::iter::repeat_n(good, 4));
    Value::Array(v)
}

fn objective(step: Step, fault: Fault) -> String {
    let worker_fault =
        |s: Step| (step == s && !matches!(fault, Fault::None | Fault::SlowExit)).then(|| fault_step(fault));
    let always = fault == Fault::FailAlways;
    let mut coder = json!({ "implement": turns(implementation(), worker_fault(Step::Implement), always) });
    let mut reviewer = json!({ "review": turns(approval(), worker_fault(Step::Review), always) });
    if fault == Fault::SlowExit {
        let slow = if step == Step::Implement { &mut coder } else { &mut reviewer };
        slow["linger_ms"] = json!(4000);
    }
    let plan = json!({ "summary": "One implementer and an independent reviewer.",
        "agents": [
            { "key": "impl", "title": "Implementer", "kind": "specialist", "responsibility": "Implement the greeting",
              "scope": ["src"], "capabilities": ["coding"], "provider": "claude_code" },
            { "key": "reviewer", "title": "Reviewer", "kind": "specialist", "responsibility": "Review the result",
              "capabilities": ["review"], "provider": "claude_code" }],
        "tasks": [
            { "key": "code", "title": "Implement the greeting", "work": "implementation", "owner": "impl",
              "scope": ["src"], "description": format!("Create src/greet.txt. {}", script(&coder)),
              "acceptance": ["src/greet.txt greets"] },
            { "key": "review", "title": "Review the greeting", "work": "review", "owner": "reviewer",
              "depends_on": ["code"], "description": format!("Review src/. {}", script(&reviewer)),
              "acceptance": ["The objective's criteria are met"] }] });
    let mut faults = serde_json::Map::new();
    if let Some(n) = step.lead_turn().filter(|_| !matches!(fault, Fault::None | Fault::SlowExit)) {
        let last = if always { n + 40 } else { n };
        for i in n..=last {
            faults.insert(i.to_string(), fault_step(fault));
        }
    }
    let lead = json!({ "supervise": { "auto_lead": plan, "faults": faults } });
    format!("Greet users. {}", script(&lead))
}

struct Fixture {
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
}

async fn fixture() -> Fixture {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "team-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    // Agents may change src/ without asking; anything else (the fault's shell command) asks.
    for category in [
        switchyard_protocol::model::OperationCategory::FileWrite,
        switchyard_protocol::model::OperationCategory::FileEdit,
    ] {
        client
            .call::<switchyard_protocol::methods::PoliciesCreate>(
                switchyard_protocol::methods::PoliciesCreateParams {
                    project_id: project.id,
                    workspace_id: None,
                    category,
                    decision: switchyard_protocol::model::PolicyDecision::Allow,
                    conditions: switchyard_protocol::model::PolicyConditions {
                        path_prefix: Some("src".into()),
                        command: None,
                    },
                    description: "agents may change src/".into(),
                    enabled: None,
                },
            )
            .await
            .unwrap();
    }
    Fixture { core, client, project_id: project.id }
}

async fn org(f: &Fixture, task: Uuid) -> OrganizationInfo {
    f.client.call::<OrganizationsGet>(TaskIdParams { task_id: task }).await.unwrap().organization.unwrap()
}

fn snapshot(o: &OrganizationInfo) -> String {
    format!(
        "status {} ({}); activity {:?}; queued {}; tasks {:?}",
        o.status,
        o.reason.as_deref().unwrap_or("-"),
        o.activity,
        o.queued_checkpoints,
        o.tasks.iter().map(|t| format!("{}={}", t.key, t.state)).collect::<Vec<_>>()
    )
}

async fn permission_requests(f: &Fixture) -> usize {
    f.client
        .call::<EventsList>(EventsListParams {
            event_types: vec!["agent.permission_requested".into()],
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
        .len()
}

/// A tool request that waits for the user's approval.
async fn pending(f: &Fixture) -> Option<Uuid> {
    f.client
        .call::<ApprovalsList>(ApprovalsListParams {
            status: Some(switchyard_protocol::model::ApprovalStatus::Pending),
            session_id: None,
            project_id: Some(f.project_id),
            limit: Some(10),
        })
        .await
        .unwrap()
        .approvals
        .first()
        .map(|a| a.id)
}

/// Waits until the team needs the user or is finished; an error when nothing
/// changes for `STALL` while it says it is running.
async fn settle(f: &Fixture, task: Uuid) -> Result<OrganizationInfo, String> {
    let mut last = String::new();
    let mut since = tokio::time::Instant::now();
    loop {
        let o = org(f, task).await;
        if matches!(o.status.as_str(), "done" | "waiting" | "cancelled" | "paused")
            || pending(f).await.is_some()
        {
            return Ok(o);
        }
        let now = snapshot(&o);
        if now != last {
            last = now;
            since = tokio::time::Instant::now();
        } else if since.elapsed() > STALL {
            return Err(format!("stalled for {}s: {last}", STALL.as_secs()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_case(step: Step, fault: Fault) {
    let mut f = fixture().await;
    let task = f
        .client
        .call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Greet users".into(),
            description: Some(objective(step, fault)),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec!["src/greet.txt greets".into()],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id;
    f.client
        .call::<WorkflowsStart>(WorkflowsStartParams {
            task_id: task,
            overrides: Default::default(),
            workspace_id: None,
            definition: Some("organization".into()),
            lead: Some(RoleBinding { provider: "claude_code".into(), model: None }),
        })
        .await
        .unwrap();

    // Faults that strike a held turn: wait for its approval request, then strike.
    if matches!(fault, Fault::Restart | Fault::CancelSubtask | Fault::StopStep | Fault::RestartTwice) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while permission_requests(&f).await == 0 {
            let o = org(&f, task).await;
            assert!(tokio::time::Instant::now() < deadline, "the faulty turn never held: {}", snapshot(&o));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if matches!(fault, Fault::Restart | Fault::RestartTwice) {
            f.core.restart().await;
            let (client, _rx) = f.core.connect().await;
            f.client = client;
            if fault == Fault::RestartTwice {
                settle(&f, task).await.unwrap();
                f.core.restart().await;
                let (client, _rx) = f.core.connect().await;
                f.client = client;
            }
        } else if fault == Fault::StopStep {
            let key = step.subtask().expect("only a specialist's step can be stopped");
            let sub = org(&f, task).await.tasks.iter().find(|t| t.key == key).unwrap().task_id.unwrap();
            let a = f
                .client
                .call::<switchyard_protocol::methods::TasksGet>(TaskIdParams { task_id: sub })
                .await
                .unwrap()
                .assignments
                .into_iter()
                .find(|a| a.ended_at_ms.is_none())
                .expect("the held step's assignment");
            f.client
                .call::<switchyard_protocol::methods::TasksCancelAssignment>(
                    switchyard_protocol::methods::TaskAssignmentIdParams {
                        assignment_id: a.id,
                        reason: None,
                    },
                )
                .await
                .unwrap();
        } else {
            let key = step.subtask().expect("only a subtask can be cancelled");
            let sub = org(&f, task).await.tasks.iter().find(|t| t.key == key).unwrap().task_id.unwrap();
            f.client
                .call::<TasksCancel>(TaskDecisionParams { task_id: sub, note: Some("not like this".into()) })
                .await
                .unwrap();
        }
    }

    let mut interventions = 0;
    let mut waits: Vec<String> = Vec::new();
    let verdict: Result<(), String> = loop {
        let o = match settle(&f, task).await {
            Ok(o) => o,
            Err(e) => break Err(e),
        };
        if let Some(approval) = pending(&f).await.filter(|_| o.status == "running") {
            // The user approves a tool request (it counts as an action).
            interventions += 1;
            waits.push("a tool request waits for approval".into());
            if interventions > MAX_INTERVENTIONS {
                break Err(format!("needed more than {MAX_INTERVENTIONS} user actions; waits: {waits:#?}"));
            }
            f.client
                .call::<ApprovalsDecide>(ApprovalsDecideParams {
                    approval_id: approval,
                    decision: ApprovalDecision::Approve,
                    note: None,
                })
                .await
                .unwrap();
            continue;
        }
        match o.status.as_str() {
            "done" => break Ok(()),
            "cancelled" => break Err(format!("the team was cancelled: {}", snapshot(&o))),
            _ => {}
        }
        let reason = o.reason.clone().unwrap_or_default();
        if fault == Fault::FailAlways && o.status == "waiting" {
            let said = format!("{reason} {}", o.question.as_deref().unwrap_or(""));
            break if said.contains("usage limit") {
                Ok(())
            } else {
                Err(format!("the team waits without naming the cause (the usage limit): {said}"))
            };
        }
        if waits.iter().filter(|w| **w == reason).count() >= 1
            && o.question.is_none()
            && !o.tasks.iter().any(|t| t.state == "proposed")
        {
            break Err(format!("resuming led back to the same wait (a dead end): {reason}"));
        }
        waits.push(reason);
        interventions += 1;
        if interventions > MAX_INTERVENTIONS {
            break Err(format!("needed more than {MAX_INTERVENTIONS} user actions; waits: {waits:#?}"));
        }
        let proposed = o.tasks.iter().find(|t| t.state == "proposed").map(|t| t.key.clone());
        let acted = if let Some(key) = proposed {
            // New work outside the plan waits for the user: approve it.
            f.client
                .call::<switchyard_protocol::methods::OrganizationsDecideScope>(
                    switchyard_protocol::methods::OrganizationsDecideScopeParams {
                        task_id: task,
                        key,
                        approve: true,
                    },
                )
                .await
                .map(|_| ())
        } else if o.question.is_some() {
            f.client
                .call::<OrganizationsAnswer>(OrganizationsAnswerParams {
                    task_id: task,
                    text: "Start it afresh.".into(),
                })
                .await
                .map(|_| ())
        } else {
            f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.map(|_| ())
        };
        if let Err(e) = acted {
            break Err(format!("the user's action was refused ({}): {}", e, snapshot(&o)));
        }
    };
    if fault == Fault::None && interventions > 0 {
        panic!("without a fault the team needed the user: {waits:#?}");
    }
    if let Err(why) = verdict {
        panic!("{step:?} / {fault:?}: {why}\nwaits on the way: {waits:#?}");
    }
}

macro_rules! cases {
    ($($name:ident: $step:ident / $fault:ident;)*) => {$(
        #[tokio::test]
        async fn $name() {
            run_case(Step::$step, Fault::$fault).await;
        }
    )*};
}

cases! {
    plan_fails: Plan / Fail;
    plan_crashes: Plan / Crash;
    plan_without_directive: Plan / NoBlock;
    plan_interrupted_by_restart: Plan / Restart;

    implement_fails: Implement / Fail;
    implement_crashes: Implement / Crash;
    implement_without_report: Implement / NoBlock;
    implement_interrupted_by_restart: Implement / Restart;
    implement_cancelled_by_user: Implement / CancelSubtask;

    assess_implementation_fails: AssessImplementation / Fail;
    assess_implementation_crashes: AssessImplementation / Crash;
    assess_implementation_without_directive: AssessImplementation / NoBlock;
    assess_implementation_interrupted_by_restart: AssessImplementation / Restart;

    review_fails: Review / Fail;
    review_crashes: Review / Crash;
    review_without_report: Review / NoBlock;
    review_interrupted_by_restart: Review / Restart;
    review_cancelled_by_user: Review / CancelSubtask;

    assess_review_fails: AssessReview / Fail;
    assess_review_crashes: AssessReview / Crash;
    assess_review_without_directive: AssessReview / NoBlock;
    assess_review_interrupted_by_restart: AssessReview / Restart;

    plan_provider_out: Plan / FailAlways;
    implement_provider_out: Implement / FailAlways;
    assess_implementation_provider_out: AssessImplementation / FailAlways;
    review_provider_out: Review / FailAlways;

    implement_stopped_by_user: Implement / StopStep;
    review_stopped_by_user: Review / StopStep;
    implement_exits_slowly: Implement / SlowExit;
    review_exits_slowly: Review / SlowExit;
    plan_restarted_twice: Plan / RestartTwice;
    implement_restarted_twice: Implement / RestartTwice;
    review_restarted_twice: Review / RestartTwice;
}

/// Without a fault the team finishes without the user.
#[tokio::test]
async fn no_fault_finishes_without_the_user() {
    run_case(Step::Plan, Fault::None).await;
}
