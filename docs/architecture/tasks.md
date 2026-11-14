# Tasks and multi-agent collaboration (milestone 6)

Tasks let the operator coordinate native agents on one piece of work. One agent investigates,
another implements, a third reviews, and Switchyard keeps the durable record. Core coordinates;
every decision stays with the operator. Decisions: ADRs 0023–0027.

## Model

```text
Project ─┬─ Task ──(depends on)──► Task          tasks, task_dependencies
         │    ├─ Assignment (role, provider, workspace) ─► Session (access: write | review)
         │    ├─ Message (structured, durable; from agent / operator / core)
         │    ├─ Finding (from reports; open → resolved)
         │    └─ Proposal (a proposed subtask; pending → approved | rejected)
         └─ Workspace (one task's open assignments at a time)
```

- **Task**: title, description, kind, priority, acceptance criteria, status, result, history. It is
  independent of sessions and survives them.
- **Assignment**: one role (`investigate`, `implement`, `review`) on one task, given to one provider
  (`claude_code`, `codex`) in one workspace. It stores the task context exactly as sent and the
  outcome. A task has at most one open assignment.
- **Session access**: implementation and investigation sessions have `write` access; review
  sessions have `review` access (file changes blocked, commands always ask).

## Lifecycle

```text
draft ─publish─► ready ◄──────────────┐          blocked (dependencies, or an agent's blocker)
                  │ assign            │ cancel/retry      ▲
                  ▼                   │                   │
               assigned ─start─► in_progress ─report─► awaiting_review ─assign review─► assigned ─► in_review
                                      │                   ▲  │  │                                   │
                                      └─session ended─► failed  └─ complete (operator) ─► completed  │
                                                          ▲                                         │
                                                          └──────────── verdict ◄────────────────────┘
```

`awaiting_review` is the operator's turn. From there the operator:

- completes the task;
- assigns a review;
- assigns another implementation (open review findings go into its context);
- fails or cancels the task.

A review verdict (`approved`, `changes_requested`, `rejected`, `blocked`) returns the task to
`awaiting_review`; it never completes it.

## One assignment, step by step

1. `tasks.assign` checks:
   - the provider is available and routes permission requests (capability `supported`);
   - the workspace belongs to the project;
   - no other task has an open assignment in that workspace;
   - the task status fits the role.

   The assignment row, a `task_assignment` message and `task.assigned` are written in one transaction.
2. `tasks.start_assignment` builds the `TaskContext` (v1) from the task, its dependencies' accepted
   results, the open findings (for an implementation), pending operator notes and the standard
   restrictions. For a review it also fingerprints the workspace. It then starts a session, or
   resumes the task's earlier session for the same role, provider and workspace, so the agent keeps
   its conversation. If Core is still stopping the session that just reported in this workspace,
   the start waits for that stop. Core never stops any other session: one the operator started or
   resumed by hand makes the start fail with `workspace_occupied`.
3. In one transaction it:
   - marks the assignment active;
   - stores the context and fingerprint;
   - sets `last_event_seq` to the journal head;
   - moves the task to `in_progress` / `in_review`;
   - marks notes delivered.

   Only then does it send the rendered context as the first message.
4. The **reactor** (a bus subscription to `agent.turn_completed`, `agent.exited`, `agent.failed`)
   handles each turn of the session:
   - it collects the turn's assistant text and parses the last `switchyard-report` block;
   - for a review, it re-scans the workspace;
   - in one transaction it advances `last_event_seq`, records the message, findings and proposals,
     ends the assignment and moves the task.

   After a final report, the session is stopped so the workspace is free for the next assignment.
5. If a turn has no usable report, it is recorded as `malformed_report` (or `status_update` for an
   interrupted turn), and the assignment stays active. The operator can send a note, which goes to
   the running session as its next message. A `question` report behaves the same way.
6. If the session ends before reporting, the assignment fails. An implementation task becomes
   `failed`, and the operator can retry it; a review returns to `awaiting_review`.

## The report block

The context asks the agent to end its final message with one block:

````text
```switchyard-report
{"type": "review", "summary": "…", "result": "changes_requested",
 "findings": [{"severity": "medium", "title": "…", "path": "src/a.rs", "line": 3, "suggestion": "…"}],
 "acceptance": [{"criterion": "…", "met": false}]}
```
````

Allowed types per role:

| Role | Allowed types |
|---|---|
| investigate | `findings`, `proposal` (with `proposed_subtasks`), `blocker`, `question` |
| implement | `implementation` (with `changed_files`, `acceptance`), `proposal`, `blocker`, `question` |
| review | `review` (with `result`), `blocker`, `question` |

Limits: 6 KiB of JSON, a 2000-character summary, 50 findings, 10 proposed subtasks. Unknown fields
are kept in `unexpected_fields` and ignored. Journaled agent text is capped at 8 KiB per message, so
the context asks for a short final message, and a report lost to truncation is recorded as such.

## Guarantees

- **No agent output authorizes anything.** Reports become messages. Findings and proposals wait for
  the operator. Only `tasks.complete` completes a task, and only `tasks.decide_proposal` creates a
  subtask (as a draft). Tool calls still go through the M4 policies; the task context grants no
  permissions.
- **Review sessions cannot change files.** The system policy denies every file mutation in a
  `review` session, and rules cannot change that. Shell commands always ask. The workspace is
  fingerprinted (full `lstat` scan) before and after the review; a difference forces the verdict to
  `blocked` and adds a high-severity finding. An approved command still runs with the user's
  permissions; this is not a sandbox.
- **Dependencies are acyclic and enforced.** Every change is checked for cycles in the same
  transaction; readiness is derived; only Core's own `dependencies:` blocks lift automatically.
- **Durable and replayable.** Every change is a transaction with its `task.*` events. The reactor
  resumes from the lowest `last_event_seq` of active assignments after a restart, and each reaction
  advances that mark atomically, so replay never applies anything twice. Sessions that did not
  survive a restart are failed by session reconciliation, and the reactor then fails their
  assignments.
- **One task per workspace.** Two tasks never write in the same worktree at the same time.

## Known limitations

- **Final message size.** Switchyard journals at most 8 KiB of each agent message. A report placed
  after a longer final message is lost; the turn is recorded as unusable, with that reason, and the
  assignment stays open.
- **Agents can ignore the format.** The report block is a request, not something Switchyard can
  enforce. A turn without a valid report is recorded as unusable, and the operator can answer with a
  note or cancel the assignment.
- **Approved review commands run with your permissions.** A review session blocks file changes
  through tools, but every shell command you approve runs as your user. It can change files or
  reach the network like any other command.
- **The workspace scan covers the workspace only.** The before/after fingerprint detects changes
  inside the worktree. It does not detect side effects elsewhere: other directories, the network,
  processes, or the Git index and refs, which live in the project's repository. An approved
  `git commit` in a review session is therefore not detected. Every final review report (verdict
  or blocker) and every end of a review session is checked.
- **One task per workspace.** Open assignments of only one task may use a workspace at a time.
- **A session end fails the work.** If an implementation's session ends without a report (an
  operator stop, a crash, a Core shutdown or restart), the task becomes `failed` and the operator
  retries it. `tasks.fail` does not apply to `ready` or `assigned` tasks; cancel those instead.
- **Reactor errors.** A failed reaction is retried a few times. If it still fails, the event is
  processed again only at the next Core start, and the task stays in progress until then.
- **Note delivery.** A note sent to a running agent is marked delivered when Core hands it to the
  session; that mark is not journaled. If the current turn ends with a final report, the session
  is stopped and the queued note is not processed.
- **Report parsing.** The block ends at the first closing fence, so a JSON string that contains
  three backticks is rejected. If the final message is truncated, an earlier quoted block can be
  parsed instead; it is still only a claim.
- **No automatic scheduling or merging.** Ready tasks are not started automatically, and workspace
  branches are never merged; the operator assigns work and integrates branches.

Review access and the policies are not an OS sandbox, and agent reports are never trusted
instructions.

## What is intentionally absent

- No swarms, no recursive spawning, and no agent-to-agent assignment.
- No automatic merge or integration of workspace branches, and no automatic approval or completion.
- No shared writable workspaces.
- No automatic decomposition: proposals need the operator.
- No scheduling or queueing of ready tasks: assignments are started by the operator.

## Code

- `crates/switchyard-tasks`: pure domain code (state machine, graph, report parser, context renderer).
- `crates/switchyard-storage/src/tasks.rs`: rows and guarded updates.
- `migrations/0005_tasks.sql`: schema.
- `crates/switchyard-core/src/tasks/`: `TaskService` (`mod.rs`), assignments (`assign.rs`) and the
  reactor (`reactor.rs`).
- `crates/switchyard-policy`: `RequestScope.review_session`.
- `apps/desktop/src/features/tasks/`: the Tasks view.
- Tests: `crates/switchyard-core/tests/tasks.rs` (fake agents, `[fake:report:HEX]` scenarios in
  `src/bin/fake_support/report.rs`); `tests/real_tasks.rs` (opt-in, paid).
