# Agent organizations (milestone 8.3)

The user states an objective once; a lead supervisor builds a team for it, delegates, checks every
result against evidence, corrects what drifts, and hands the finished work to delivery, which the user
approves. Decisions: ADRs 0035 (model, authority, selection) and 0036 (supervision and runtime).

## Model

```text
Objective (root task) ── organization@1 workflow ── organizations row (pure state)
  Organization: agents          Task graph: organization tasks           Runtime
  lead (supervisor) ──────────► objective (coordination) ── M6 task      supervisor session (read-only, resumed)
    ├─ impl (specialist) ─────► code (implementation) ───── M6 subtask ─► assignment org:code#n ─► session
    └─ docs-lead (supervisor) ► docs (coordination) ─────── M6 subtask
         └─ writer ───────────► api-docs (documentation) ── M6 subtask ─► assignment org:api-docs#n
  Shared context: org_context (append-only, versioned, verified / claim / proposal / decided / superseded)
```

- Supervisors own coordination tasks and review everything delegated under them; specialists own work.
- Every organization task is an M6 task: created through a proposal decided by Core
  (`organization:<supervisor>`), completed when its supervisor accepts it. The objective is completed only
  by delivery.

## A run

1. `workflows.start {task_id, definition: "organization", lead?}` on a ready task: Core selects the
   lead's provider/model (or takes the user's), creates the workspace, the workflow and the organization,
   and starts the lead's planning turn.
2. The lead answers with a `switchyard-org` directive: agents (with a preferred provider/model it may
   request), tasks with owners, scopes, acceptance criteria and dependencies, decisions. Core selects a
   provider/model per agent (see *Selection*), checks the limits and authority, creates the subtasks and
   starts the first ready work.
3. A specialist works through an ordinary assignment (policies, approvals, review sessions, reports).
   Its context carries `organization`: its supervisor, scope, the supervisor's instructions and the
   shared context that concerns it.
4. While it works, Core interrupts the turn if it starts to change a file outside its scope (or fails
   repeatedly). Either way, its supervisor gets a checkpoint: the report or interruption, plus the
   workspace's changed files and the out-of-scope ones, verified from Git.
5. The supervisor accepts, corrects (instructions for the next turn), reassigns, cancels, adds agents or
   tasks, records decisions, or escalates (to its own supervisor, or to the user, who answers on the
   task page).
6. A sub-supervisor plans its own scope and reports it done; its supervisor reviews that like any work.
7. When everything under the lead is accepted, it reports the objective done, naming outstanding
   issues. If the project requires a review (M7 settings), an approved review in the organization must
   have seen exactly the current content. Then M8.2 delivery runs: the user commits, authorizes and
   approves the exact commits.

## Selection

| Fact | Basis | Source |
|---|---|---|
| permission routing, resume, interrupt, queued messages, steering, streaming, usage, cost | verified | the adapter's `Capabilities` (ADR 0022) |
| models | reported | the installed CLI (`agents.models`) |
| reasoning, coding, research, review, documentation, testing, long-context; relative cost/speed | configured | the user's estimates (Projects → *Agent teams*) |

Every agent needs verified permission routing; supervisors need verified resumption. The user's
override for a skill wins; then the supervisor's request (if installed, offered and eligible); then the
best score on the user's estimates, preferring a different provider from the implementer for review
work, then lower cost. Without estimates the choice says so ("no skill estimates are configured").

## Limits (project settings, defaults)

| Limit | Default |
|---|---|
| levels of supervision | 3 |
| agents | 6 |
| tasks (objective included) | 12 |
| concurrent sessions | 1 (the only value the runtime supports) |
| corrections per task | 2 |
| automatic retries of a failed/unusable turn | 1 |
| supervision turns | 30 |
| minutes before it waits | 240 |
| cost / tokens | off; enforced where the provider reports them |

A directive that exceeds a limit changes nothing; the supervisor sees why and may escalate. Two rejected
directives in a row are retried, the third makes the organization wait. A supervision turn that fails
before it decides anything (a usage limit, an API error) is not a rejected directive
(`Input::SupervisionFailed`): the same checkpoint is retried within the retry limit, then the
organization waits and names the cause. A directive may cancel a task and add its replacement under the
same title in one go.

## The failure matrix

`crates/switchyard-core/tests/team_faults.rs` runs a small team through the real Core and the fake CLIs
with one fault at one step: every step (plan, implement, assess, review, assess) × every fault (a failed
turn, a crash, no report or directive, a restart, the user cancelling or stopping the step, a slow exit,
two restarts, a provider that fails every turn). The lead is adaptive
(`src/bin/fake_support/auto_lead.rs`): it reacts to its checkpoint as a reasonable lead would. A case
passes when the team finishes with at most three actions of a simple user policy (approve a tool
request, approve proposed work, answer, resume), or, for a provider that keeps failing, when it waits
naming the cause. It fails on a stall (no change while running), a resume that leads back to the same
wait, or more user actions. Add a case for every new failure seen in real use.

## Guarantees

- Core decides everything: agents cannot add agents outside their subtree, create work outside
  coordination tasks they own, assess work outside their scope, mark context verified, or exceed limits.
- No agent output authorizes anything: tool requests go through M4; nothing commits, approves an
  integration or pushes. The objective completes only through user-approved delivery.
- Every decision is one transaction (state, events, subtasks, context, the next assignment) committed
  before anything starts; replays change nothing; a restart makes the organization wait.
- Review work runs read-only and is bound to the exact content by M7.

## Known limitations

- Sequential: one session at a time per organization; no parallel specialists yet.
- Mid-turn steering is not used (not verified for either CLI); corrections reach the next turn.
- Scope checks cover file-changing tools; a shell command the user approves can change any file (the
  verified evidence after the turn still shows it).
- Directive quality depends on the model; the fakes in tests are scripted and prove only the machinery.
- No provider fallback; the lead's provider must be installed and resumable.

## Code

- Pure: `crates/switchyard-tasks/src/organization/` (`mod.rs` decide, `directive.rs`, `render.rs`).
- Selection: `crates/switchyard-agents/src/profiles.rs`.
- Driver: `crates/switchyard-core/src/tasks/organization.rs`; storage `crates/switchyard-storage/src/organizations.rs`;
  `migrations/0010_organizations.sql`.
- UI: `apps/desktop/src/features/workflows/OrganizationPanel.tsx`, `OrgSettingsPanel.tsx`.
- Tests: `crates/switchyard-tasks` unit tests; `crates/switchyard-core/tests/organizations.rs` (fakes).
