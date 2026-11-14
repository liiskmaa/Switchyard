# ADR 0034: The workflow delivers through M7

- Status: accepted (2026-11-15)

## Context

M8.1 stopped at "ready for delivery": the operator then had to create an integration, commit the
workspace, validate and approve in a separate view. M7 already does all of that safely (exact-commit
binding, policy-governed validation, Core-owned operations, crash recovery). M8.2 connects the two
without adding a second delivery engine and without moving any decision away from the operator.

## Decision

1. **Delivery is a stage of `engineering@1`.** An approved review moves the workflow to the
   `delivery` stage instead of finishing it. The pure decision function gains `Delivery` inputs,
   `Prepare`/`Validate`/`CompleteTask` actions and a final `delivered` status (migration 0009 rebuilds
   `task_workflows` to allow it and adds `integration_id`).
2. **M7's record is the state.** The workflow stores only which integration it delivers through.
   Every delivery fact is derived from that integration (status, blockers, runs for the bound commit,
   the policy's decision per command): `needs_commit`, `awaiting_validation_authorization`,
   `validating`, `validation_failed`, `blocked`, `awaiting_approval`, `integrating`, `delivered`,
   `delivery_failed`, `cancelled`. Nothing comes from an agent.
3. **What runs by itself**: creating the integration (or adopting the open one of the workflow's
   workspace), preparing it again, and starting validation *only when every enabled command is
   `allow`* — once per source commit. Validation goes through `IntegrationService::validate`, so the
   policy evaluation, the recorded configuration and the pass rule are M7's.
4. **What stays the operator's** (unchanged rules):
   - **Committing the reviewed work** (ADR 0029: workspace changes only on operator request). The
     workflow waits at `needs_commit`; the task page offers *Commit reviewed changes*.
   - **Authorizing `ask` commands** for the run, bound to their exact command lines (ADR 0030).
     `deny` never runs; delivery is blocked with the reason.
   - **Approving the exact commits** (`integrations.approve`, ADR 0028/0029). A passed validation or
     an approved review never integrates anything.
   - Resolving conflicts or a moved/dirty target (M7's own actions; the workflow waits as `blocked`).
5. **Failed validation** waits; the operator either runs it again or sends the work back
   (`workflows.request_correction`, with a note for the agent): a correction round, a new review, and
   delivery continues on the new commit with the same integration.
6. **Completion follows delivery.** When M7 reports `integrated`, the workflow becomes `delivered`
   and Core completes the task in the name of the operator who approved the integration, with the
   merge commit in its result. A task is never completed before its integration landed.
7. **Durability.** Delivery inputs go through the same transaction as every workflow decision
   (state, events, processed journal seq). Creating an integration is idempotent: an existing open
   integration for the workspace is adopted. Validation is started at most once per source commit
   automatically. At startup, delivering workflows are re-read from their integration after M7's own
   recovery; operations M7 owns (validation, approval) are never repeated by the workflow.

## Consequences

- One task: agents work, the operator commits, authorizes (if the policy asks) and approves; the
  rest happens by itself, and the task page shows all of it.
- The commit click is a real extra step. Removing it means changing ADR 0029 (for example: Core
  may commit exactly the tree a review approved). That is a separate decision.
- A project without validation commands is integrated without validation after the operator's
  explicit approval; the approval view says so.
- The delivered task is completed by Core on the approver's behalf; a task completed manually during
  delivery cancels the workflow.
