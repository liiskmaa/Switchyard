# Recorded Codex app-server sessions (codex-cli 0.166.0)

Real `codex app-server --listen stdio://` sessions recorded on 2026-11-12 (macOS, ChatGPT sign-in)
in a throwaway repository, with a research client that sent `initialize`, `initialized`,
`account/read`, `thread/start` (or `thread/resume`) and one `turn/start` per prompt, and answered
approval requests as listed below. `*.stdout.jsonl` is what the app-server printed.

Sanitised before committing:

- Only server → client lines are kept.
- The `account/read` response (account identity) is removed, and later response ids are
  renumbered by −1 so they match the adapter, which never sends `account/read`.
- `account/updated`, `account/rateLimits/updated`, `remoteControl/status/changed` (host name,
  installation id) and `mcpServer/startupStatus/updated` notifications are removed.
- The repository path is `/workspace/demo`, other temporary paths `/workspace/…`, the home directory
  `/home/user`, and the `initialize` user agent is replaced.

Every other line is unmodified app-server output.

| Fixture | Thread configuration | Scenario |
|---|---|---|
| `approvals` | `untrusted`, `read-only` | 4 turns: `cat notes.txt` asks → declined; a read outside the workspace is refused by the model without a tool call; `apply_patch` adding `hello.txt` asks (`item/fileChange/requestApproval`, no paths) → accepted; `touch` asks → declined |
| `interrupt` | `untrusted`, `workspace-write` | `turn/interrupt` 5 s into a streaming essay → `turn/completed` `interrupted` |
| `interrupt_pending_approval` | `untrusted`, `workspace-write` | A command approval left unanswered, then `turn/interrupt` → `turn/completed` `interrupted`, then `serverRequest/resolved` for the open request |
| `resume` | `thread/resume` in a new process | The thread recalls a command output from the earlier process; a replayed `thread/tokenUsage/updated` for the previous turn comes first |
| `subagent` | hardening flags, `agents.max_threads=1` | `spawn_agent` creates a child thread anyway; the child's command approval arrives with the child's `threadId` → declined. The recording stops after the child's `turn/completed` (the research client took it for the root's) |
| `hardened_tools` | Switchyard's hardening flags | No web search or MCP tools; a command approval answered with `cancel` declines it and interrupts the turn |
