# ADR 0039: Core runs the tests before every review

- Status: accepted (2026-11-22)

## Context

Every change should be covered by unit, integration and regression tests, and by browser tests when
it changes the UI, and the full set should run as part of review. Reviews are read-only and
fingerprinted (ADR 0027): a reviewer running tests would change the workspace (build output) and
its verdict would be blocked, and its report of the results would only be a claim. The operator
chose that Switchyard runs the tests, and that the agents decide which commands run them.

## Decision

1. The implementer's `implementation` report names its test commands: `"tests": [{kind, command
   (argv), name?, timeout_secs?}]`, kinds `unit`, `integration`, `regression`, `browser`.
2. After an implementation step, the workflow enters stage `testing` and Core runs the **test
   gate** in the workflow's workspace:
   - required kinds: unit, integration, regression; browser too when Git shows UI files changed
     (by extension) since the workspace's base. A missing kind fails the gate without running;
   - each command is evaluated by the policy exactly like a validation command (shell, the exact
     command line, the project's rules and auto mode): `deny` fails the gate, `ask` makes the
     workflow wait until the operator resumes (which authorizes those commands for that run);
   - commands run without a shell, one after another, with the validation runner (process group,
     timeout, output tails).
3. The run (commands, policy decisions, results) and the workflow's decision are committed in one
   transaction (`workflow_test_runs`, migration 0012, append-only). Passing tests start the review;
   failing ones go to reasoning (ADR 0038) like review findings. Results reach reasoning and the
   reviewer as `test_results` (context v3), labelled as Switchyard's own run.
4. A restart while the tests run makes the workflow wait (resume runs them again); nothing is
   rerun automatically.

## Consequences

- Test commands come from an agent, so they run only what the policy allows or the operator
  authorizes. With auto mode on (ADR 0037) they run without asking; an allowed command runs with
  the user's permissions.
- The reviewer stays strictly read-only; test results cannot be faked by an agent.
- M7 validation before delivery is unchanged (project-configured commands for the exact commit).
