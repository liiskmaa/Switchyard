# ADR 0045: A watchdog for stuck work

- Status: accepted (2026-11-23)

## Context

Work got stuck in ways each needing its own fix: a question nobody saw, a team resuming into the
same wait over and over ("Resume does nothing"), a step whose agent was gone, an approval left
for an hour. The operator asked for one place that notices and deals with it.

## Decision

1. **Detection is pure** (`switchyard_tasks::watchdog::check`, configurable `Limits`): from a
   snapshot of one task's workflow or team, built by Core from its own records (status and
   reason, the open step and whether its session is alive, its last activity, the oldest pending
   approval, the open question, and the journal's resumes and the waits that followed them):
   - `resume_bounces`: two resumes in 30 minutes that went back to the same wait within 10 s;
   - `orphaned_step`: an open step whose session is gone for over a minute;
   - `silent_agent`: no agent activity for 20 minutes while working;
   - `approval_waiting` (10 minutes) and `question_waiting` (30 minutes).
2. **Core looks every 30 s** (`tasks/watchdog.rs`), keeps the current findings in memory
   (`watchdog.list`), and journals every change (`task.stuck_changed`, with what it did).
3. **Only safe remedies are automatic**, each once:
   - an orphaned workflow step is ended (cancelled), as a Core restart would leave it, so Resume
     runs it again;
   - a team whose resumes bounce is handed to its lead (`organization::Input::Nudge`): the
     reason becomes a verified observation and a checkpoint, unless the team waits for the user's
     answer.
   Everything else is shown under **Needs you** and on Home with what helps (open the agent's
   session, ask the supervisor, stop), never a Resume that would bounce.

## Consequences

- Findings are not stored: after a restart the watchdog finds them again within one interval.
- The watchdog never stops a session, answers a permission request or approves anything.
