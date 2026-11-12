# ADR 0018: Policies and human approvals

- Status: accepted (2026-11-11)

## Decision

### One path for every request

`AgentService::on_permission_requested` is the only consumer of adapter permission requests.

1. **Classify** the request, outside any lock (the adapter's classifier plus Core's path facts).
2. **Evaluate and record** in one SQLite transaction:
   - load the applicable active rules and the current global revision;
   - run the pure evaluator;
   - insert the `policy_evaluations` row and the journal events.
3. **Act:** after the commit, depending on the decision:
   - **allow**: deliver allow to the agent; no approval row is created;
   - **ask**: create the approval in the same transaction (`evaluation_id` links them), move the
     session to `waiting_for_approval`, and start the expiry timer, exactly as before;
   - **deny**: deliver a denial whose message carries the explanation; no approval row is created.
4. **Fail closed:** if classification, rule loading, evaluation or the commit fails, Core delivers a
   denial. Nothing is allowed unless its evaluation is durably recorded first.

### Human decisions

- Approve or deny settles that one approval. It never creates, edits or suggests a rule.
- There are no "allow for this session" or "always allow" buttons, and Claude's
  `permission_suggestions` are still ignored.
- A `deny` evaluation has no approval row, so there is nothing to approve.

### Pending approvals and policy changes

A pending approval stays bound to the evaluation that created it (its revision and its explanation).
Policy changes can make a pending approval **more restrictive, never less**:

- After every rule change, Core re-evaluates all pending approvals of that project using the stored
  operation and path facts:
  - **`deny` now:** the approval is settled `denied`, with source `policy`, and the agent receives a
    denial that explains why. A new evaluation row (trigger `reevaluation`) records it.
  - **`ask` or `allow` now:** the approval stays pending. A pending approval is never approved
    because a rule now allows it; the person who was asked still decides.
- `approvals.decide {approve}` re-evaluates once more inside its transaction, which closes the race
  with a concurrent rule change. If the result is now `deny`:
  - the approval is settled `denied` (`policy`);
  - the call fails with `policy_denied`;
  - the evaluation is recorded with trigger `decision_check`.
- A human *deny* is always accepted, because it is never less restrictive.

The alternative, re-evaluating and auto-approving, was rejected because a pending request would then
change outcome without anyone looking at it. Keeping the original `ask` and ignoring later denials
was also rejected, because an operator who just blocked something would still see it offered for
approval.

### Unchanged protections

- Expiry still denies.
- Cancellation by the agent, agent exit, Core shutdown and Core restart still settle approvals as
  `cancelled` or `denied`.
- Decisions are recorded exactly once, through the SQL guard.
- Shutdown still tells the agent "deny".

All of these paths are unaffected by policies, and none of them can produce an allow.

### Audit

The trail answers "who asked for what, under which policy, and why this outcome":

- **`policy_evaluations`** (append-only):
  - who asked: project, workspace, session, agent, adapter, tool, native request id;
  - what was asked: the stored operation and its path facts;
  - which policy: global revision, system version, matched rules with their revisions, and the
    deciding rule;
  - why: the decision, a reason code, and the explanation;
  - when, by which trigger, and the approval it created or settled.
- **`approvals`:** the human decision (who, when, note), or the timeout, cancellation or `policy`
  source.
- **Journal events:**
  - `agent.permission_evaluated` for every evaluation;
  - `agent.permission_requested` and `agent.permission_resolved` for approvals, as before;
  - `policy.rule_changed` for every rule change.
- **`policy_rule_revisions`** (append-only): every version of every rule.
