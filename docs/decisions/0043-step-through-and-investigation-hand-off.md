# ADR 0043: Step-through hand-offs, and the investigation travels with the work

- Status: accepted (2026-11-22)

## Context

Hand-offs between roles looked slow. Measured on real runs, Switchyard's own part is seconds
(next step created at once, session ready in 1–11 s); the time is each agent's turn, and a role's
first session spends much of it exploring the repository again: coding and review got the
investigation's structured findings but not its summary, details, or what it had looked at. (Later
rounds of a role already continue that role's session.) The operator also asked to see, and
influence, what one role hands to the next.

## Decision

1. **Context v4: `investigation`.** An implementation or review step gets the task's latest
   reported investigation (not a triage): its `summary` and `details` (a claim) and `files_read`,
   the workspace files its session opened during that step, read by Core from the journal's
   `agent.tool_started` events (Claude's `file_path`/`path`, ACP `locations`; workspace-relative,
   inside the workspace only, at most 40). The prompt tells the role to start from them.
2. **Step-through mode** (per workflow, off by default; `workflows.set_step_through`,
   `workflow.step_through_changed`): before each automatic hand-off to a role, the workflow waits
   with `Resume::Start` instead of starting the step. The operator sees the next step and its
   exact prompt (`workflows.preview_next`, built by the same code that starts a step, pending notes
   included), may add a note (it goes into that prompt), and continues. Operator-started steps
   (resume, supervisor routes, corrections) are not held. Not for organizations: their lead
   decides hand-offs.

## Consequences

- A role still checks the investigation's claims; `files_read` is a starting point, not a limit.
- Step-through adds a wait per hand-off by design; "Continue without stopping" turns it off.
