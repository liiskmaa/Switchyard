# ADR 0042: Auto mode runs everything except releases; approvals follow you

- Status: accepted (2026-11-22); amends ADR 0037
- System policy version: 4

## Context

With auto mode on, the operator still had to approve shell commands of read-only sessions
(reviews, organization supervisors) and background commands, and had to open the Approvals page
to do it. The operator asked for auto mode to allow every action except git commits, pushes and
deployments, and to answer approvals wherever they are in the app.

## Decision

1. **Auto mode allows**, with no matching rule: file changes and shell commands inside the
   workspace (as before), **shell commands of review sessions**, and **background commands**.
   Review sessions still cannot change files (system deny), and Core still fingerprints a
   review's workspace and blocks a verdict when it changed.
2. **Releases always ask** in auto mode (`release_action` cap): `git commit`, `git push`,
   package publishing (`npm`/`pnpm`/`yarn`/`bun publish`, `cargo publish`, …), `run`/`make`
   targets named deploy/release/publish/ship, container pushes, cluster and infrastructure
   changes (`kubectl apply`, `helm upgrade`, `terraform apply`, `pulumi up`, …), hosting deploys
   (`vercel`, `netlify deploy`, `fly deploy`, `firebase deploy`, `wrangler deploy`, cloud CLIs'
   deploy commands), GitHub releases and merges, `ssh`, and `scp`/`rsync` to a remote host.
   `switchyard_policy::release_action` scans every word of the command line (also inside
   `sh -c "…"` and after `&&`, `|`, `;`), so a mention in a `grep` pattern asks too: a false
   positive costs one question, a miss would cost an unapproved release.
3. An **exact-command allow rule** the operator wrote still decides for its command (e.g. allow
   `git push` in one project). Block and ask rules win as before.
4. Still capped in auto mode: paths outside the workspace or unresolvable, `.git` writes, unknown
   input fields. Still blocked: network and external tools, everything the system denies.
5. **Approvals follow the operator**: while any request is pending, a panel in the corner of
   every page (except Approvals) shows up to three of them with Approve and Deny, through the
   same card and the same Core path as the Approvals page.

## Consequences

- A review's shell command can change files despite the read-only role; for reviews the
  fingerprint catches it, for organization supervisors the evidence (from Git) shows it.
- Auto mode is detection by text, not a sandbox: an agent can still release through a script
  whose name does not say so. Block rules and branch protection on the remote remain the real
  guard. Switchyard itself never pushes.
