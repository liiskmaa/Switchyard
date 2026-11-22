# ADR 0038: Reasoning decides on review findings; no correction role

- Status: accepted (2026-11-22)

## Context

In `engineering@1` a review that requested changes went straight to a correction round with its
own role binding. Reviews can be wrong, or ask for changes that are not worth making; sending every
finding to implementation wastes rounds and can make the code worse. The operator wants findings
to flow back to reasoning, which either agrees (fix) or disagrees (back to the reviewer), and no
separate "correction" role.

## Decision

1. **`engineering@2`.** A review that requests changes starts a reasoning step: an investigation
   (`investigate#n+1`, stage `triage`, the investigation binding) whose context carries the open
   findings and `review_triage: true`. It answers with a new report type, `triage`, with
   `"decision": "fix"` or `"dispute"` and its reasoning in `summary`/`details` (no findings of its
   own). The assignment's outcome is `triage: fix` / `triage: dispute`.
2. **Fix** → the next implementation round (`implement#n+1`); its context carries the findings and
   reasoning's answer (`reasoning`). **Dispute** → the review again (`review#n+1`) without any
   change; the reviewer's context carries the reasoning and asks for a verdict again.
3. Fixes and disputes share the existing two automatic rounds; then the workflow waits and the
   operator breaks the tie (resume lets reasoning decide once more).
4. **No correction role.** Fixes are implementation rounds with the implementation binding.
   `Binding::Correct`, `Stage::Correction` and the stored `correct` binding remain only so earlier
   states load and resume.
5. Workflows started before (stored state without `review_triage`) keep the old routing.
6. A reasoning report is a claim like any other: it never approves or completes anything.

## Consequences

- One more agent turn per round with findings, in exchange for fewer needless changes and a
  visible argument when reviewer and reasoning disagree.
- Task context v3 (`review_triage`, `reasoning`).
