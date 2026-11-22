# Workflows (milestone 8.1; engineering@2)

One task, one *Start*: Switchyard coordinates the agents through investigation, implementation,
tests and review, loops back through reasoning when something is found, and stops when the result
is ready for your delivery decision. Decisions: ADR 0033 (the workflow), ADR 0038 (reasoning on
findings, no correction role), ADR 0039 (the test gate), ADR 0040 (subtasks).

## `engineering@2`

```text
            ┌───────────── dispute ─────────────┐
            ▼                                    │
 Task → Reasoning ── plan/fix ──► Coding ──► Tests ──► Review ──► Delivery (M7) ──► branch
            ▲                                │        │
            └──────── failed tests ──────────┘        │
            ▲                                          │
            └──────────── findings ────────────────────┘
```

| Step | Runs as | Binding | Next |
|---|---|---|---|
| `investigate#1` | investigate | investigation | a plan of 2–8 subtasks → the subtasks in turn; otherwise → `implement#1` |
| `implement#n` | implement | implementation | usable report → **test gate** |
| test gate | Core | — | all required kinds pass → `review#n`; failed / missing / blocked → `investigate#n+1` |
| `review#n` | review (read-only) | review | `approved` → delivery (or the next subtask); `changes_requested` → `investigate#n+1` |
| `investigate#n` (n > 1) | investigate (triage) | investigation | `triage: fix` → `implement#n`; `triage: dispute` → `review#n` (implementation skipped) |

- **Reasoning on findings (ADR 0038).** A review's findings and failed tests go to reasoning first
  (stage `triage`, the investigation binding). It answers with a `triage` report: `fix` (the next
  implementation gets its reasoning with the findings) or `dispute` (the reviewer gets the
  reasoning and reviews again without any change). Fixes and disputes share the 2 automatic
  rounds; then the workflow waits for you to break the tie.
- **No correction role.** A fix is another implementation round with the implementation binding
  (`implement#n`). A stored `correct` binding is ignored for new steps.
- **Test gate (ADR 0039).** The implementer's report names its test commands
  (`"tests": [{"kind": "unit|integration|regression|browser", "command": [...]}]`). Core runs them
  in the workspace, without a shell, before the review: each goes through the policy like a
  validation command (`allow` runs, `ask` waits for your resume, `deny` fails the gate). Unit,
  integration and regression tests are required; browser tests too when the changed files (read
  from Git) include UI files (`.tsx`, `.jsx`, `.vue`, `.svelte`, `.html`, `.css`, …). Results are
  recorded (`workflow_test_runs`) and passed to reasoning and the reviewer as verified evidence.
- **Subtasks (ADR 0040).** When the first investigation proposes 2–8 subtasks, they become the
  workflow's plan (not proposals to decide). Each subtask runs implementation → tests → review →
  reasoning in turn (`s2/implement#1`, `s2/review#1`, …) with its own rounds; a subtask is done
  when its review approves; after the last one the task goes to delivery once. More than 8 waits
  for you.
- Workflows started before engineering@2 keep their old routing (their stored state has no
  `review_triage`/`test_gate`).

### When it stops for you (`waiting`)

| Situation | Resume does |
|---|---|
| Review still requests changes (or tests still fail) after 2 rounds | reasoning decides once more |
| Test commands your policies ask about | authorize and run them (or turn on auto mode) |
| Core restarted while the tests ran | run the tests again |
| Reasoning planned more than 8 subtasks | implement the task as one piece |
| Review `rejected` | an implementation round |
| Review `blocked` (e.g. the workspace changed during it) | the review again (`review#n@2`) |
| An agent reports a blocker | the step again |
| No usable report after 2 automatic repair notes | another repair note |
| An agent asks a question | continue (answer with a note; it continues when the agent reports) |
| A session failed twice (one automatic retry, same provider and model) | the step again |
| You cancelled the step's assignment | the step again |
| Core restarted while the step ran | the step again (agents are never restarted automatically) |

Approved review → **delivery** (M8.2, ADR 0034), shown on the task page:

| Delivery phase | What happens | Your part |
|---|---|---|
| preparing | Core creates the task's integration (or adopts the open one) | — |
| needs_commit | the reviewed work is uncommitted | *Commit reviewed changes* (Switchyard never commits) |
| validating | policy-`allow` commands run by themselves, once per commit | — |
| awaiting_validation_authorization | some commands are policy-`ask` | authorize them for this run |
| validation_failed | the run did not pass | *Run validation again* or *Send back for correction* |
| blocked | conflicts, a moved or dirty target, a denied command, … | resolve it, then *Check again* |
| awaiting_approval | exact source and target commits, diff, validation, what approving does | *Approve these commits…* → *Integrate* |
| integrating / delivered | M7 integrates; then the task is completed in the approver's name | — |

Without validation commands the approval says nothing was validated. A delivered workflow is final;
the merge commit is on the task.

## Roles and models

- **Project roles** (Projects → *Workflow roles*): provider and model for investigation &
  reasoning, implementation and review. There is no separate correction role (ADR 0038).
- **Task overrides** (New task → *Override roles and models for this task*, or the task's *Start
  workflow*): only the roles you change.
- The effective roles are **snapshotted** when the workflow starts.
- **Model choices** come from the installed CLI: Claude Code's `--model` aliases named in its
  `--help` (2.1.325: `fable`, `opus`, `sonnet`) or a full `claude-…` name; Codex's `model/list`
  (0.166.0). *CLI default* is an explicit choice. The model is passed to the CLI exactly; the model
  the CLI reports is shown next to it. An unlisted model is refused before anything starts.
- Every role needs a provider that routes permission requests to Switchyard. Implementation and
  review on the same provider is allowed with a warning.

## Guarantees

- **No duplicate steps.** Each step is an assignment with a unique key (`task_id`, `step_key`). The
  decision, its events, the processed journal position and the step's assignment are one
  transaction; the session starts after the commit. Replays, restarts and concurrent operator
  actions cannot create a step twice.
- **Same rules as manual work.** Steps go through the operator's assignment path: policies,
  approvals (shown inline on the task), read-only reviews with before/after scans, workspace rules.
- **Bounded.** 2 correction rounds, 2 repair notes, 1 retry; then a human decides.
- **Takeover.** Pause or cancel the workflow, then assign manually (*Advanced* on the task).

## Known limitations

- Sequential: one step at a time per task; no parallel subtasks (M8.3).
- Committing the reviewed work is a click (ADR 0029); the target branch is the project checkout's
  branch when the integration is created.
- No stage timeouts: a turn that never ends waits for you (M8.4).
- No provider fallback.
- Whether a full Claude model name exists is only known when the CLI starts; an unknown one fails
  the session (retried once, then it waits).
- The task status moves through `awaiting_review` between automatic steps; the UI labels it as
  internal and shows the workflow stage.

## Code

- Decisions (pure): `crates/switchyard-tasks/src/workflow.rs`.
- Driver: `crates/switchyard-core/src/tasks/workflow.rs`; assignment creation shared with the
  operator in `tasks/assign.rs`.
- Model choices: `crates/switchyard-agents/src/models.rs`, `codex/models.rs`.
- Storage: `crates/switchyard-storage/src/workflows.rs`, `migrations/0008_workflows.sql`.
- UI: `apps/desktop/src/features/workflows/`.
- Tests: `crates/switchyard-core/tests/workflows.rs` (fakes); real CLIs' model lists:
  `real_codex_lists_its_models`, `real_claude_names_its_model_aliases` (opt-in, no paid turns).
