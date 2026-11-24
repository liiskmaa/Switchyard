# ADR 0046: Keeping a team on its objective

- Status: accepted (2026-11-23), in stages

## Context

A team asked to "let each user connect their own mailbox" finished that at 23:04, then spent
2.5 hours on work it added itself (privacy of derived views), with one task attempted seven times.
Nothing stopped it: the lead may plan freely, and turns that end in a question or without a usable
report did not count toward any limit.

## Decision

### Stage 1: rules Core enforces (no model judgement)

1. **Agreed scope.** The first breakdown of a coordination task (the objective, or an area handed
   to a sub-supervisor) is agreed. After that, new **implementation** work (or a new delegated
   area) must name in `continues` an agreed task it finishes or fixes; investigation,
   documentation, testing and review support the objective without widening what the product does
   and are exempt (step-by-step planning stays possible). A coordination area added later may be
   created, but it is marked late: its own breakdown is not agreed, so implementation it plans needs
   `continues` or the user too. Anything else is created as **proposed**: it never starts,
   the team keeps working on agreed tasks, and the user approves or declines it
   (`organizations.decide_scope`, `organization.scope_proposed`). The lead is told the decision.
2. **Loop guard.** A task that would start a 4th work turn (`max_task_turns`, default 3) without
   being accepted, or an 9th supervision turn in a row without accepting anything
   (`max_turns_without_progress`, default 8), makes the team wait for the user with the reason.
   Every turn counts, including ones that ended in a question or without a report. The user's
   Resume allows exactly one more attempt and resets the count.
3. The lead's prompt states the rules and the count of turns without progress.

### Stage 2: an independent goal check

4. **When.** After the lead accepts work, and at least every 30 minutes while the team runs
   (`goal_check_due`), Core's watchdog loop starts one check per team (none while one runs; after a
   check without a usable verdict, the next try waits 10 minutes).
5. **Who.** A fresh read-only session (never resumed, never the task supervisor's conversation) in
   the task's side workspace, on a provider other than the lead's (Gemini, then Codex, then Claude
   Code); only if no other is installed, the lead's own provider, and the report says so.
6. **What it sees.** Its own prompt (`organization::goal::render_prompt`): the objective and the
   user's done-when criteria (or "derive at most five" when there are none), every task with its
   state, turns and whether it was proposed or late, the turns without progress, the last decisions
   (marked as claims), and the actual changes from Git. A criterion is met only if the changes show
   it. It answers with one `switchyard-goal` block: verdict `done`, `on_track`, `drifting` or
   `looping`, a summary, each criterion's status with evidence, and the off-goal task keys.
   The block is checked against the team (unknown fields, unknown keys, "done" with an open
   criterion are refused).
7. **Effect** (`Input::GoalChecked`, journaled as `organization.goal_checked`): the report is stored
   with the team and noted as a claim. `on_track` does nothing. `done` tells the lead to finish now
   or name the open criterion. `drifting`/`looping` stop a running team for the user with the
   summary and the off-goal tasks. A waiting team keeps its reason. The verdict never completes,
   approves, cancels or creates anything.

### Stage 3: progress against the objective in the UI

8. The team page shows "Progress toward the goal": each criterion met/partly/open with its evidence
   from the last check (the user's criteria as "not checked yet" before the first), the verdict and
   when and by whom it was checked, the off-goal tasks, and the supervision turns without accepted
   work (`OrganizationInfo.goal`, `turns_since_progress`).
   The goal keeper is drawn on the team graph beside the user (dashed: it is not an agent of the team
   and never works on a task), with a dashed line to the lead labelled by its last verdict, who judges,
   and its live state from `OrganizationInfo.goal_keeper`: checking now, first check due, last verdict,
   a failed check, or off when no agent CLI is available. A check you cannot see might as well be off.
   A verdict block with a misspelled tag (a referee wrote `switchyard-go`) is still read; its JSON passes
   the same checks.

## Consequences

- The lead cannot grow the work beyond the objective without the user; it can still re-plan agreed
  work (`continues`).
- A legitimate but slow task may hit the loop guard; one Resume continues it.
- The goal check costs one extra agent turn per accepted result (or half hour). It is a model's
  judgement: it can be wrong, so it only stops the team for the user or nudges the lead.
- The referee runs in the side workspace, which reflects the task branch when it was created; it
  judges from the diff Core gives it, not from that checkout.
