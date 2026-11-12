# ADR 0021: Codex permission mediation and sandbox configuration

- Status: accepted (2026-11-12)

## Context

Codex decides on its own what needs approval, depending on `approvalPolicy` and its OS sandbox
(`read-only`, `workspace-write`, `danger-full-access`). Switchyard requires one central decision path
(ADR 0016–0018). What the real CLI 0.166.0 does was measured, not assumed:

- With `approvalPolicy: "untrusted"`, every command (including `cat`, `ls`, `pwd`, `git status`) and
  every patch produced an approval request. Nothing ran unasked.
- An approved command or patch can run outside the sandbox: both wrote to `/Users/Shared`.
- Inside the `workspace-write` sandbox, network access failed even for an approved command.
- MCP servers, plugins, web search, browser and computer use, image generation and hooks are tools
  that act without approval requests. Sub-agent spawning cannot be switched off.

## Decision

- Launch every thread with `approvalPolicy: "untrusted"` and `sandbox: "workspace-write"`, and disable
  the tools above with `--disable …` and `-c web_search="disabled" -c mcp_servers={}`.
- Map command approvals to `shell`, network approvals to `network`, patches to `file_write`,
  `file_edit` or `file_delete` by change kind; everything else is `unknown`. Mixed-kind patches,
  moves, patches whose item was not seen, and requests from other threads are unclassified (denied).
- Never trust Codex's parsed `commandActions` to classify a command as a read.
- Answer allow with `accept` and deny with `decline`. Never `acceptForSession`, never a policy
  amendment, never a weaker sandbox or approval policy.
- Unsupported server requests (permission grants, MCP elicitation, user input, dynamic tools, unknown
  methods) are evaluated as `unknown` (recorded, denied) and answered negatively.
- Codex items that appear without passing through an approval (e.g. a sub-agent call) are recorded
  with a warning.

## Consequences

- Complete mediation of shell commands and patches, verified against 0.166.0; reads ask by default.
- An approved Codex operation has the user's privileges, exactly like an approved Claude tool use.
  Policies are still not a sandbox.
- A future Codex release could add tools that act without approval; the version check marks such
  releases untested, and unexpected item types are journaled as warnings.
