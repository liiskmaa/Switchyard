# Agent integration notes: Claude Code and OpenAI Codex

Status: **research only.** No adapter is implemented in milestone 1.

Researched 2026-11-11. Sources:

- Claude Code: the official docs at `code.claude.com/docs/en/{headless,cli-reference,hooks,permissions,settings,sessions,agent-sdk/*}`.
- Codex: the official docs at `developers.openai.com/codex/*` (now redirecting to `learn.chatgpt.com/docs/*`) and `github.com/openai/codex/codex-rs/app-server/README.md`.
- Local verification against the installed CLIs: **Claude Code 2.1.322** (`claude --help`) and
  **codex-cli 0.166.0** (`codex app-server --help`, plus `codex app-server generate-json-schema`, which
  produced 106 client methods, 85 notifications and 10 server requests).

Labels used below:

- **[stable]**: documented with no maturity caveat.
- **[experimental]**: the vendor labels it experimental, beta, alpha or under development.
- **[undocumented]**: observed but not found in official docs. Do not depend on it without a guard.

Facts change quickly with these tools. Re-verify against the installed version (the CLI's own `--help`
and generated schemas) at adapter start-up, and record the detected version in `agents.cli_version`.

---

## 1. Claude Code

### Execution model

- **Headless mode [stable]:** `claude -p/--print` runs non-interactively. Exit code is 0 on success
  and non-zero on failure. In-run failures (such as missing auth) are printed as the result on stdout;
  invalid flags go to stderr before the run starts.
- **Structured output [stable]:** `--output-format text|json|stream-json`. `stream-json` emits
  newline-delimited JSON. Every documented example also passes `--verbose`; the docs do not say
  whether it is required. Test this at integration time.
- **Structured input [stable flag, partially documented]:** `--input-format stream-json` keeps
  stdin open for multiple user messages and control requests. The full stdin wire schema is **not
  documented** in the CLI docs. It is described only indirectly through the Agent SDK
  (`control_request {type, request_id, request}`; responses echo `request_id`).
- `--include-partial-messages` (stream-json only) adds `stream_event` lines carrying text deltas
  (`event.delta.type == "text_delta"`).
- `--replay-user-messages`, `--include-hook-events` and `--prompt-suggestions` are stream-json
  extras.
- `--max-turns` and `--max-budget-usd` are print-mode limits. The budget is a client-side estimate
  and can be exceeded.
- `--json-schema` produces structured final output in `result.structured_output`.

### stream-json message types

| `type` | Notes |
|--------|-------|
| `system` / `init` | model, tools, MCP server status, `mcp_server_errors`, plugins, `capabilities` (e.g. `interrupt_receipt_v1`, `interrupt_cancel_queued_v1`). The full field list is not documented. |
| `system` / `api_retry` | attempt, max_retries, retry_delay_ms, error category |
| `system` / hook events, `permission_denied`, … | Hook events only with `--include-hook-events` |
| `assistant`, `user` | `parent_tool_use_id` is null on the main thread and set inside subagents |
| `stream_event` | Partial deltas (with `--include-partial-messages`) |
| `result` | Always the last line. `subtype: success \| error_max_turns \| error_during_execution \| error_max_budget_usd \| error_max_structured_output_retries`, plus `is_error`, `result`, `session_id`, `num_turns`, `duration_ms`, `total_cost_usd` (an estimate), `usage`, `modelUsage`, `permission_denials` |

### Sessions [stable]

- `--session-id <uuid>` lets **Switchyard choose the ID**. This is the preferred approach, because
  the ID is known before the process starts.
- `--resume <id>`, `--continue` (`-p --continue` is needed for print-mode sessions), and
  `--fork-session` (a new ID when resuming). `--no-session-persistence` disables saving.
- Transcripts are stored at `~/.claude/projects/<cwd with non-alphanumerics → '-'>/<session-id>.jsonl`
  (or under `CLAUDE_CONFIG_DIR`) and retained for 30 days by default. **The JSONL format is internal
  and may change.** Do not parse it; rely on the stream instead.
- Resuming does **not** restore `--mcp-config`, `--settings`, `--add-dir` or `--plugin-dir`. The
  adapter must pass them again.
- 2.1.322 also has `--bg/--background` with `claude agents|attach|logs|stop`. This is Claude's own
  background-session manager. Switchyard must **not** use it, because Core is the process owner.

### Permissions

- `--permission-mode default(manual)|acceptEdits|plan|auto|dontAsk|bypassPermissions` [stable]. Since
  v2.1.302, `-p` starts in Manual mode, where calls needing approval are **denied** unless a
  PermissionRequest hook or `--permission-prompt-tool` approves them.
- `--allowedTools` / `--disallowedTools` rules use the form `Tool` or `Tool(specifier)`
  (`Bash(git diff *)`, `Read(//abs/**)`, `WebFetch(domain:x)`, `mcp__server__tool`). Evaluation order
  is deny, then ask, then allow; the first match wins. Prefix rules are **not a security boundary**
  (per the docs).
- `--permission-prompt-tool mcp__<server>__<tool>` [stable] delegates each approval decision to an
  MCP tool. **This is the natural hook for Switchyard approvals:** Core exposes an MCP server, Claude
  asks it, and Core turns each request into an `approvals` row that the operator answers. The exact
  required return format is documented on the errors page, which was **not fetched**. Confirm it
  before implementing.
- `--permission-prompts host|none` (v2.1.259+).
- Settings precedence: managed > `--settings`/CLI > project local > project shared > user. Lists
  merge. Project `permissions.allow` applies only to trusted folders.

### Hooks [stable, except agent-type hooks which are experimental]

- Events include SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PermissionDenied,
  PostToolUse, PostToolUseFailure, Notification, SubagentStart/Stop, Stop, StopFailure, PreCompact,
  PostCompact, FileChanged, WorktreeCreate/Remove, SessionEnd and more. The full list is in the docs
  table.
- Handler types are command, http, mcp_tool and prompt; `agent` is experimental. Input is JSON on
  stdin with `session_id`, `cwd`, `hook_event_name`, `permission_mode`, `tool_name`, `tool_input`,
  `tool_use_id`, and so on. Exit code `2` blocks. JSON output can return `permissionDecision`
  (allow/deny/ask/defer) or `decision: "block"`.
- `-p` without `--bare` runs the project's `.claude/settings.json` hooks and `.mcp.json` servers
  **without a trust dialog**. That is a security consideration for repositories Switchyard opens.
- `--bare` skips hooks, CLAUDE.md, plugins and keychain, and requires `ANTHROPIC_API_KEY`. The docs
  say it will become the default for `-p` in a future release. The adapter must pin its behaviour
  explicitly rather than rely on the default.

### Interruption

- **SIGINT** ends the current turn cleanly [stable].
- **SIGTERM** makes the process exit with code 143: the turn is left unfinished, the Bash process tree
  is killed, and SessionEnd hooks run [stable].
- An in-band `interrupt` control request exists via stream-json input. The protocol is documented
  only through the SDK (`Query.interrupt()`, gated by the `interrupt_receipt_v1` capability).

### Agent SDK

`@anthropic-ai/claude-agent-sdk` (TypeScript) and a Python SDK **wrap the same CLI binary** as a
subprocess. Anthropic's terms do not allow third-party products to offer claude.ai login through the
SDK. Products should use API-key auth. **Implication:** Switchyard drives the user's own installed
`claude` CLI as the user (local-first, the user's own credentials). This needs a product and legal
check before distribution.

---

## 2. OpenAI Codex

### Options

| Surface | Maturity | Use for Switchyard |
|---------|----------|--------------------|
| `codex app-server` (JSON-RPC over stdio) | **[experimental]**: the CLI help says "[experimental] Run the app server"; the docs say it "may change without notice" | Best fit: long-lived threads, streaming items, server-initiated approvals, steering, interruption |
| `codex exec --json` (JSONL events) | [stable] (`exec`, `exec resume`, `exec fork`) | Fallback: one-shot turns with read-only or pre-approved sandboxes; no interactive approvals |
| Codex SDK | Recommended by the docs for CI and automation | Not evaluated in depth (TypeScript/Python). The core is Rust, so the app-server is preferred. |

### App Server protocol

- **Transport:** stdio (default, `--listen stdio://`), JSONL framing, JSON-RPC 2.0 semantics **with
  the `"jsonrpc":"2.0"` field omitted** on the wire. WebSocket (`ws://`) and `unix://` listeners exist
  and are labelled experimental and unsupported. Bounded queues return error `-32001` ("Server
  overloaded; retry later").
- **Handshake:** an `initialize` request with `{clientInfo: {name, title, version}, capabilities?}`,
  then an `initialized` notification. Capabilities include `experimentalApi`,
  `optOutNotificationMethods`, and others. Calls gated by experimental status fail without
  `experimentalApi`.
- **Primitives:** Thread → Turn → Item.
  - Thread: `thread/start` (cwd, model, approvalPolicy, sandbox, …), `thread/resume`, `thread/fork`,
    `thread/list`, `thread/read`, `thread/archive`, `thread/unsubscribe`, `thread/revert`, …
  - Turn: `turn/start` (threadId, input, cwd, approvalPolicy, sandboxPolicy, model, effort,
    outputSchema, …), **`turn/steer`** (threadId, input, expectedTurnId), **`turn/interrupt`**
    (threadId, turnId → the turn completes with status `interrupted`), `review/start`.
- **Notifications:** `thread/started`, `thread/status/changed`, `turn/started`, `turn/completed`
  (status `completed|interrupted|failed`, plus `inProgress` in the schema enum), `turn/diff/updated`,
  `turn/plan/updated`, `item/started`, `item/completed` (authoritative final state), deltas
  (`item/agentMessage/delta`, `item/commandExecution/outputDelta`, `item/reasoning/*Delta`),
  `thread/tokenUsage/updated`, `serverRequest/resolved`, `error`, `warning`.
- **Item types:** userMessage, agentMessage, plan, reasoning, commandExecution, fileChange,
  mcpToolCall, dynamicToolCall, webSearch, contextCompaction, review markers, and others.
- **Approvals (server → client requests):**
  - `item/commandExecution/requestApproval`: accept \| acceptForSession \| decline \| cancel \|
    acceptWithExecpolicyAmendment.
  - `item/fileChange/requestApproval`: accept \| acceptForSession \| decline \| cancel.
  - `item/permissions/requestApproval`: a granted subset, with scope `turn|session`.
  - `mcpServer/elicitation/request`.
  - The local schema also lists legacy `applyPatchApproval` and `execCommandApproval` (decision enum
    `approved` and others).
  - Each request is followed by `serverRequest/resolved`.
  - **This is the key integration point:** Core answers these requests on the operator's behalf, or
    according to central policy.
- **Persistence:** threads are stored as JSONL rollouts in Codex's sessions directory (the path is
  not documented). Resume by `thread/resume {threadId}`.
- **Schema:** `codex app-server generate-json-schema` and `generate-ts` (both experimental) produce
  version-specific definitions. The adapter should generate schemas in CI against a pinned Codex
  version and generate its Rust types from them.

### Sandbox and approval values

- The CLI and config use `sandbox_mode` values `read-only | workspace-write | danger-full-access`.
  The App Server `sandboxPolicy.type` values are camelCase (`readOnly`, `workspaceWrite`,
  `dangerFullAccess`, `externalSandbox`).
- `approval_policy`: the current docs say `on-request | never`, and say that `untrusted` is
  **retired** and may prevent start-up. **Discrepancy:** the 0.166.0 generated schema still lists
  `untrusted | on-request | never | {granular: {...}}`. Treat `untrusted` as unsupported.
- Network access is off by default in `workspace-write`.

### `codex exec --json` [stable]

JSONL events: `thread.started` (thread_id), `turn.started`, `turn.completed` (usage), `turn.failed`,
`item.started` and `item.completed` (agent_message, reasoning, command_execution, file_change,
mcp_tool_call, web_search, plan updates), and `error`. The full item schema and the exit codes are
**not documented**. `codex exec resume <id>|--last` continues a session. There is no in-band approval
channel, so only the sandbox and approval policy decide.

---

## 3. Implications for Switchyard

1. **Process ownership:** Core spawns both CLIs as children (never `claude --bg` or the Codex daemon),
   with `cwd` set to the agent's workspace. It records `pid` and the vendor session or thread ID in
   `sessions`, and supervises them (exit codes, signals).
2. **Claude adapter:** `claude -p --input-format stream-json --output-format stream-json --verbose
   --session-id <uuid> --permission-mode default --permission-prompt-tool mcp__switchyard__approve
   --mcp-config <core-provided> --strict-mcp-config`. Core serves a small MCP server (stdio or UDS) for
   approvals and, later, inter-agent messaging.
3. **Codex adapter:** `codex app-server` over stdio, using JSON-RPC types generated from the pinned
   schema, with server-initiated approvals mapped to Switchyard approvals and `turn/steer` and
   `turn/interrupt` exposed as operator controls. Because it is experimental, version-pin it and
   feature-detect methods.
4. **Event volume:** delta notifications (token streams, command output) are high-frequency.
   Journaling every delta with `synchronous=FULL` is unnecessary. A likely design: journal item
   start and completion, approvals and lifecycle events; stream deltas through an ephemeral channel.
   This is an open decision (see `decisions.md`).
5. **Security:** both tools execute repository-provided configuration (Claude hooks and MCP; Codex
   `.codex` config). Workspaces opened by Switchyard should start with restrictive defaults, and the
   operator should see which project-level agent configuration exists.
6. **Version drift:** detect versions at start-up (`claude --version`, `codex --version`) and refuse
   untested major versions, or run them in a degraded mode.
