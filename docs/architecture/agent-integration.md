# Agent integration: Claude Code and OpenAI Codex

**Status:** Claude Code has been integrated since milestone 2, Codex since milestone 5 (section 2
points to its own document).

**Evidence labels**

- **[verified]**: observed by running the installed CLI. The transcripts are committed in
  `crates/switchyard-agents/tests/fixtures/claude-2.1.322/`, and the opt-in test
  `crates/switchyard-core/tests/real_claude.rs` replays the behaviour through Core.
- **[documented]**: stated in the official docs, but not exercised by Switchyard.
- **[undocumented]**: used by the official Agent SDK but not described in the CLI docs. Switchyard
  relies on this only where it is verified, and guards against drift.
- **[assumption]**: not verified. Call it out wherever it matters.

Sources: <https://code.claude.com/docs/en/cli-reference>, `/headless`, `/hooks`, `/permissions`,
`/sandboxing`, `/sessions`, `/agent-sdk/permissions`, `/agent-sdk/user-input`, and
`/agent-sdk/streaming-vs-single-mode`. The research was done on 2026-11-11.

---

## 1. Claude Code (CLI 2.1.322, tested 2026-11-11)

### 1.1 Launch command (exactly what Core runs)

```text
claude -p --input-format stream-json --output-format stream-json --verbose \
       --include-partial-messages \
       --permission-mode default --permission-prompt-tool stdio \
       --restricted --strict-mcp-config \
       --tools Read,Write,Edit,Glob,Grep,Bash,NotebookEdit \
       --settings '{"permissions":{"ask":["Read","Write","Edit","Glob","Grep","Bash","NotebookEdit"],"defaultMode":"default"}}' \
       --session-id <uuid>            # new session; or: --resume <native id>
       [--model <alias>]
```

- **Launch:** the process is started with `execve`. No shell is involved, and every argument is a
  separate argv entry (`crates/switchyard-agents/src/claude/launch.rs`).
- **Working directory:** the session's workspace (a Core-owned worktree; see section 1.5). The process runs in its own process group,
  and stdin, stdout and stderr are all piped.
- **Environment:** the child environment is built from scratch (`ENV_POLICY`):
  - Passed through: `HOME`, `USER`, `PATH` (with the CLI's directory prepended), locale, proxy and
    CA settings, `SSH_AUTH_SOCK`, and provider variables (`ANTHROPIC_*`, `AWS_*`, `CLAUDE_CODE_USE_*`,
    `CLAUDE_CONFIG_DIR`, `GOOGLE_APPLICATION_CREDENTIALS`).
  - Removed: everything else, including the variables an enclosing Claude Code session sets
    (`CLAUDECODE`, `CLAUDE_CODE_SESSION*`, and so on).
  - Authentication is the user's own (keychain via `HOME`, or provider variables). Switchyard never
    stores credentials and never logs the environment.
- **Detection** (`detect.rs`):
  - Locates the CLI from the explicit configuration, then `SWITCHYARD_CLAUDE_BIN`, then `PATH`, then
    `~/.local/bin`, `~/.claude/local`, `/opt/homebrew/bin` and `/usr/local/bin`.
  - Runs `--version` and checks that every required flag is mentioned in `--help`. In 2.1.322
    `--permission-prompt-tool` is not listed as an option, only referenced in the description of
    `--permission-prompts`.
  - Runs `auth status --json` and reads only `loggedIn`, `authMethod` and `apiProvider`. The email
    and organisation fields in that output are ignored.

### 1.2 Process and stream behaviour

| Question | Answer | Evidence |
|----------|--------|----------|
| Multiple messages in one process | Yes. Each stdin user message starts a turn, and each turn ends with exactly one `result`. | [verified] `write_allow` |
| Output before the first input | None. `system/init` is emitted when the first message arrives, and **again at the start of every turn**. | [verified] |
| Switchyard chooses the session ID | Yes, with `--session-id <uuid>` (a valid UUID is required). | [verified] |
| Resume | `--resume <id>` continues the conversation **under the same `session_id`**, with earlier context remembered. | [verified] `resume`, `real_claude.rs` |
| Message sent while a turn runs | Queued. It becomes the next turn and does **not** change the running turn. | [verified] `queued_message`; [documented] streaming mode |
| Steering a running turn | **Not supported** by this interface. `capabilities.steer_running_turn = false`. | [verified] by the queue behaviour |
| Interrupt | `{"type":"control_request","request_id":…,"request":{"subtype":"interrupt"}}` is answered with `control_response {still_queued: []}`, a `user` message `[Request interrupted by user]`, and a `result` with `subtype:"error_during_execution"` and `terminal_reason:"aborted_streaming"`. **The process stays alive.** | [verified] `interrupt`; [undocumented] wire format (SDK `interrupt()`) |
| Interrupt during a pending permission | The CLI sends `control_cancel_request {request_id}`, then a rejected `tool_result`, then `terminal_reason:"aborted_tools"`. | [verified] `interrupt_pending_permission` |
| stdin closed | Exits after the current turn. The exit code is **1 if the last `result` was an error**, otherwise 0. | [verified] |
| SIGTERM | Exit code 143; the turn is left unfinished. | [documented] headless |
| stderr | Diagnostics only. Not structured. | [verified] |

### 1.3 stdout message types used

Examples are abbreviated, verbatim shapes from the fixtures.

```json
{"type":"system","subtype":"init","cwd":"/workspace/demo","session_id":"79affdf0-…","tools":["Bash","Edit",…],
 "mcp_servers":[],"model":"claude-haiku-5-5","permissionMode":"default","claude_code_version":"2.1.322",
 "capabilities":["interrupt_receipt_v1","interrupt_cancel_queued_v1",…]}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"DONE"}},
 "session_id":"…","parent_tool_use_id":null}
{"type":"assistant","message":{"id":"msg_…","content":[{"type":"tool_use","id":"toolu_01…","name":"Write",
 "input":{"file_path":"/workspace/demo/notes.txt","content":"switchyard"}}]},"session_id":"…"}
{"type":"control_request","request_id":"894d46da-…","request":{"subtype":"can_use_tool","tool_name":"Write",
 "display_name":"Write","input":{…},"description":"notes.txt","tool_use_id":"toolu_01…",
 "permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}]}}
{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01…","type":"tool_result",
 "content":"File created successfully at: …"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"DONE","session_id":"…","num_turns":2,
 "duration_ms":4736,"total_cost_usd":0.004,"stop_reason":"end_turn","terminal_reason":"completed",
 "permission_denials":[]}
```

- Cost on `result`: `total_cost_usd` is the **running total for the session**. It grows with
  every turn and carries across `--resume`, so it is not the cost of the turn. The turn's own cost
  is the sum of `costUSD` over `model_usage_since_last_result` (one entry per model). An aborted
  turn reports `{}` there and repeats the previous total. Switchyard journals both: `cost_usd` is
  the session total and `turn_cost_usd` is the turn's cost, absent when unreported.
- `assistant` lines carry **one content block each** (thinking, text or tool_use). The text is
  complete, so Switchyard journals it once as `agent.message` and does not journal each delta.
- Other message types seen and ignored: `system/status`, `system/thinking_tokens` and
  `rate_limit_event`. `stream_event` covers `message_start`, `content_block_start`,
  `content_block_delta` (`text_delta`, `thinking_delta` and `input_json_delta`),
  `content_block_stop`, `message_delta` and `message_stop`.
- Parsing is lenient: unknown types and fields are ignored, malformed lines become `agent.warning`,
  and oversized lines (over 16 MiB) are discarded without losing alignment with the stream.

### 1.4 Permission mechanism (why stdio, not an MCP prompt tool)

`--permission-prompt-tool stdio` makes the CLI send each permission decision to the host as a
`can_use_tool` control request on stdout. The host answers on stdin:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<id>",
  "response":{"behavior":"allow","updatedInput":{…original input…}}}}
{"type":"control_response","response":{"subtype":"success","request_id":"<id>",
  "response":{"behavior":"deny","message":"<shown to Claude as the tool_result>"}}}
```

Both forms were [verified]. On deny, Claude receives a `tool_result` with `is_error: true`, the
message text, and an entry in `permission_denials`. Neither stdio delegation nor this response shape
is described in the CLI docs [undocumented]. It is the transport that the official Agent SDK uses for
`canUseTool`, whose allow/deny shapes are [documented].

The documented alternative, `--permission-prompt-tool mcp__server__tool`, was not chosen:

- Its return format lives on a docs page that could not be retrieved.
- It requires Switchyard to run an MCP server process for every session.
- The CLI waits up to `MCP_TIMEOUT` (30 s) for that server before the first turn.

Mitigation for relying on undocumented behaviour: the tested version is recorded with each session,
and the fixtures and fake agent pin the shapes. An incompatible CLI fails visibly (start-up failure
with stderr, or `agent.warning`) instead of silently allowing anything.

**Which requests reach Switchyard** ([documented], `/agent-sdk/permissions`). The CLI evaluates, in
order: hooks, then deny rules, then ask rules, then the permission mode, then allow rules, then the
host (`can_use_tool`). Only calls that no earlier step resolves reach Switchyard. Core makes sure
nothing earlier can *allow* a call:

- `--restricted` ignores user, project and local settings files, so their allow rules and their
  hooks do not apply. Managed (enterprise) settings still apply; that is a known limitation.
- `--permission-mode default` is used. `acceptEdits`, `bypassPermissions` and
  `--dangerously-skip-permissions` are never used.
- `--settings` adds an `ask` rule for **every enabled tool** (milestone 4, ADR 0017). Each Read,
  Write, Edit, Glob, Grep, Bash and NotebookEdit call reaches Switchyard as `can_use_tool`:
  - [verified] `bash_ask`: with the Bash `ask` rule, `ls` and `cat /etc/hosts` both produced
    `can_use_tool`, although Claude treats them as read-only.
  - [verified 2026-11-11, 2.1.322] with `ask` rules for Read, Glob and Grep, a Read of a file in the
    working directory, `Glob *.txt` and `Grep hello` each produced `can_use_tool`. Without those
    rules, Claude allows them silently [documented].
  - The requests carry `tool_name`, `display_name`, `input`, `description`, `tool_use_id`,
    `decision_reason_type`, `decision_reason_rule_source` and `server_policy_ask_only`.
    Switchyard keeps the decision reason in the approval details and does not depend on it.
- **Reads outside the working directory** never reach Switchyard: [verified] `--restricted` makes
  Claude deny a Read of `../outside.txt` itself (it appears in `permission_denials`).
- `--strict-mcp-config` loads no MCP servers. Without it, account-level claude.ai connectors were
  loaded [verified].

**Policy evaluation.** Core classifies every `can_use_tool` request into an adapter-neutral operation
(`switchyard_agents::claude::classify`). It evaluates the operation against the project's and the
workspace's rules and records the result, and only then answers:
- `allow` and `deny` are answered at once, with no approval;
- `ask` creates an approval for a person.

Unknown tools, tools that are not enabled and malformed input are denied. Built-in defaults:
- reads and searches inside the workspace are allowed (Claude allowed these silently before
  milestone 4);
- writes, edits and Bash ask;
- network and MCP tools are denied.

See ADRs 0016–0019 and [policies.md](policies.md).

**Hooks were evaluated and not used for interception.** PreToolUse, PermissionRequest and similar
hooks are shell commands that Claude runs. Using them would mean Switchyard writing executable
configuration into repositories or `--settings`. With `--restricted`, project hooks are ignored
anyway. Lifecycle observation comes from the stream itself (`init`, `tool_use`, `tool_result`,
`result`), which carries the same information without an extra process per event. Hooks remain an
option for future needs such as `PostToolUse` diffs.

**Timeouts and unavailability.** The CLI has no documented timeout for `can_use_tool`; it waits
indefinitely [verified: the requests stayed pending]. Switchyard therefore enforces its own timeout
(15 min by default, `SWITCHYARD_APPROVAL_TIMEOUT_SECS`), after which it **denies** the request. If
Core cannot record a request, it denies it immediately. If Core dies, the CLI's stdin closes and the
process exits without running the tool.

### 1.5 Filesystem scope

- **The working directory is a workspace**, never the project checkout: a Core-owned Git worktree
  on `switchyard/<workspace-id>` under `<data dir>/workspaces/` (see
  [workspaces.md](workspaces.md)). Core validates it before every launch or resume and passes its
  exact path as the child's `cwd`. The adapter gets only that path; it has no workspace logic.
  Claude stores sessions per working directory, so `--resume` runs in the same workspace.
- `--restricted` confines the file tools to the working directory (and `--add-dir`, which is never
  passed) [documented]. Reads and writes outside it are refused by the CLI itself [verified for
  Read]. Inside it, every tool use is decided by Switchyard's policy.
- **Bash is not sandboxed by Claude by default** [documented: "the sandbox is off by default"].
  Switchyard compensates by requiring an operator decision for every Bash command, unless a rule
  allows that exact command (see above). It
  does *not* claim OS-level isolation. A command the operator approves runs with the user's
  privileges and can reach other workspaces and the original checkout: **worktrees isolate source
  trees, not processes.**
- Registering the Switchyard repository itself as a project grants nothing special. It gets the same
  confinement and approvals as any other project.

### 1.6 Known unsupported or limited behaviour

- **Steering a running turn:** not possible. Messages can only be queued (they start the next turn),
  or the turn can be interrupted.
- **`updatedInput` (approve with modifications):** supported by the CLI. Switchyard always passes the
  original input and does not offer editing.
- **Sub-agent (`Task`) output:** excluded from the tool list until Switchyard supervises nested
  agents.
- **Live output after a disconnect:** not replayed. Completed messages are in the journal, and the
  in-progress message is available as a snapshot (`sessions.get.live_text`).
- **Recovering a running process after a Core crash:** not possible. See section 3.

## 2. OpenAI Codex

Implemented in milestone 5 through `codex app-server` (JSON-RPC over stdio), verified against
`codex-cli` 0.166.0. The milestone-1 research that stood here (surfaces, `codex exec`, open
questions) is superseded by [codex-integration.md](codex-integration.md), which records the verified
protocol, the permission mediation, the security boundary and the capability differences. The
recorded sessions are in `crates/switchyard-agents/tests/fixtures/codex-0.166.0/`.

Section 3 applies to Codex sessions as well: they are supervised by the same Core lifecycle (one
process per session, stop and shutdown escalation, no automatic restart, explicit resume with
`thread/resume`).

## 3. Process ownership and recovery (implemented policy)

| Situation | Behaviour | Evidence |
|-----------|-----------|----------|
| Desktop closes or disconnects | Agents keep running. On reconnect the UI lists the same session (same pid) and its pending approvals. No new process is created. | `tests/agents.rs::reconnecting_client_…` |
| Duplicate start | At most one live session per **workspace** (`workspace_occupied`); several per project, each in its own workspace. Resuming an attached session fails with `conflict`. | `one_live_session_per_workspace…`, `workspaces.rs::two_sessions_work_concurrently…`, `stop_then_resume…` |
| Graceful Core shutdown (SIGTERM, SIGINT or `system.shutdown`) | Pending approvals are cancelled (`core_shutdown`) and the agent is told to deny. A running turn is interrupted. stdin is closed, followed by SIGTERM and then SIGKILL after grace periods (5 s + 5 s). The session ends `stopped` with failure `core_shutdown`. | `core_shutdown_stops_sessions…`, `process.rs` |
| Core crash (SIGKILL) | The agent's stdin pipe closes, so the CLI exits by itself; no orphan remains. On restart, sessions left live are marked `failed` (`core_restarted`) and their pending approvals `cancelled` (`core_restart`). **The running process is not recovered.** The workspace and all its changes are kept; it returns to `ready`/`dirty` (ADR 0014). | `process.rs::core_crash_leaves_no_orphan…`, `workspaces.rs::restart_reconciles…` |
| Recovery | Explicit only: `sessions.resume` starts a new process with `--resume <native id>` in the session's workspace. It is never automatic, and is refused while a process is attached or for pre-workspace (legacy) sessions. | same, `workspaces.rs::legacy_sessions…` |
| Agent crash | The session becomes `failed`, with the exit code or signal and the last 1 KB of stderr. Pending approvals are cancelled (`agent_exited`). | `crash_is_reported…` |
