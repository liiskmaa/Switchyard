# Product

## Register

product

## Users

People who direct AI coding agents on their own projects but do not live in Git, terminals or policy
files. They describe an outcome, choose which AI does which job, answer the agents' permission
requests, and decide what gets delivered. They use Switchyard on a desktop (macOS), often next to
other work, checking in when something needs them rather than watching every step.

Experienced developers use it too, but the default view must make sense without knowing Git
plumbing, worktrees, policy rules or Switchyard's internal states. Expert detail stays one step away,
never in the way.

## Product Purpose

Switchyard is a local multi-agent engineering workstation. One task, coordinated agents, validated
implementation, human-approved delivery: the user states what they want once; Switchyard runs
investigation, implementation, review, correction and validation, and brings the result back for an
explicit decision. Success: the user always knows what is happening, what needs them, and what will
happen when they press a button, without reading logs.

## Brand Personality

Calm, trustworthy, clear. Quietly capable: confident enough not to shout. The feel of a well-made
Apple app (Things, Apple's own utilities): generous space, one obvious next action per screen, plain
language, nothing decorative.

## Anti-references

- **Admin / ops consoles**: walls of key-value tables, every internal field and ID on screen,
  bordered boxes inside bordered boxes, internal state names as user-facing text.
- **Flashy AI products**: gradients, glowing accents, chat-bubble everything, hype copy, "magic".
- **CI dashboards (Jenkins-style)**: logs and status noise first, the decision buried at the bottom.

## Design Principles

1. **What needs me comes first.** Every screen leads with the current state and the one decision
   waiting for the user; history and internals follow, collapsed.
2. **Plain words over system words.** Say "Ready to approve" and "Changes need a commit", not
   `awaiting_review`, `needs_commit` or worktree jargon. Technical identifiers are available, not
   foregrounded.
3. **Progressive disclosure.** Show the summary; reveal diffs, logs, policy reasoning and IDs on
   request. Power lives one click deeper, never removed.
4. **Every action states its consequence.** Before anything changes code, branches or permissions,
   say exactly what will happen, in one sentence.
5. **Quiet by default.** Color, weight and motion signal state and urgency only. If everything is
   emphasized, nothing is.

## Accessibility & Inclusion

WCAG 2.2 AA: text and UI contrast, full keyboard operation with visible focus, accessible names for
every control (the app is also driven through macOS Accessibility). Respect `prefers-reduced-motion`.
Never rely on color alone to convey state.
