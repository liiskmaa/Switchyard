# Workflows (milestone 8.1)

One task, one *Start*: Switchyard coordinates the agents through investigation, implementation,
review and correction, and stops when the result is ready for your delivery decision. Decision:
ADR 0033.

## `engineering@1`

| Step | Runs as | Binding | Next |
|---|---|---|---|
| `investigate#1` | investigate | investigation | any usable report → `implement#1` |
| `implement#n` | implement | implementation (n = 1), correction (n > 1) | usable report → `review#n` |
| `review#n` | review (read-only) | review | `approved` → **ready for delivery**; `changes_requested` → `implement#n+1` (while n ≤ 2) |

A correction gets the open review findings in its task context, and continues the implementer's
previous session when the provider, model and workspace are the same and the CLI can resume.

### When it stops for you (`waiting`)

| Situation | Resume does |
|---|---|
| Review still requests changes after 2 correction rounds | one more correction round |
| Review `rejected` | a correction round |
| Review `blocked` (e.g. the workspace changed during it) | the review again (`review#n@2`) |
| An agent reports a blocker | the step again |
| No usable report after 2 automatic repair notes | another repair note |
| An agent asks a question | continue (answer with a note; it continues when the agent reports) |
| A session failed twice (one automatic retry, same provider and model) | the step again |
| You cancelled the step's assignment | the step again |
| Core restarted while the step ran | the step again (agents are never restarted automatically) |

Approved review → `completed`: the task stays *awaiting review*; prepare, validate and approve the
integration under Delivery, and complete the task yourself. The workflow never does either.

## Roles and models

- **Project roles** (Projects → *Workflow roles*): provider and model for investigation,
  implementation, review and correction (correction defaults to implementation).
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
