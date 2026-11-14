# ADR 0025: Structured messages, the switchyard-report block and a versioned task context

- Status: accepted (2026-11-13)

## Context

Agents must receive the same, inspectable instructions every time, and Core must turn their answers
into structured, durable records without trusting them. Neither CLI offers a structured "result"
channel that both support, and free text cannot be acted on safely.

## Decision

- **Task context.** Core renders a versioned `TaskContext` (v1: task, role, acceptance criteria,
  project, workspace path and branch, dependencies with result summaries, open review findings,
  operator notes and the restrictions Switchyard enforces) into the first message of the assignment
  (`switchyard_tasks::render`, deterministic). The JSON exactly as sent is stored with the
  assignment. The context grants nothing: it says so, and every tool call still goes through the
  policies.
- **Report block.** The agent ends its final message with one fenced ` ```switchyard-report ` JSON
  block. Core parses the last such block of the turn (assistant messages since the last processed
  event, plus the turn's result text), with limits (6 KiB, 2000-character summary, 50 findings, 10
  proposed subtasks) and a per-role allowlist of report types: investigate → `findings`,
  `proposal`, `blocker`, `question`; implement → `implementation`, `proposal`, `blocker`,
  `question`; review → `review`, `blocker`, `question`. Unknown fields are kept in
  `unexpected_fields` and ignored.
- **Messages.** Every report becomes a `messages` row (`findings`, `proposal`, `completion_report`,
  `review_result`, `blocker`, `info_request`), with sender, session, causation (the journal event of
  the turn) and correlation (the task). A missing, malformed, oversized or wrong-role report is
  recorded as `malformed_report` with an excerpt, and the assignment stays active. A `question`
  keeps it active too. Operator notes are messages as well; they are delivered to the running
  session at once, or with the next assignment's context.
- **Never an authorization.** A message is a claim or a request. It cannot approve a tool call,
  start, assign, complete or create anything. Findings and proposals wait for the operator; a review
  verdict never completes a task.

## Consequences

- Agent behaviour is auditable: the context sent, the raw report and the derived state are all stored.
- Journaled agent text is capped at 8 KiB per message, so a report after a very long final message
  is lost. The context asks for a short final message, and Core records this case explicitly.
- A real agent can ignore the format. Core then records an unusable report, and the operator can
  answer with a note.
