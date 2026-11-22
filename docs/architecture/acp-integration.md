# ACP integration: Gemini CLI (Phase 0 findings)

Status: **implemented** (ADR 0044, `crates/switchyard-agents/src/acp`). Phase 0 findings below; the
real end-to-end test is `crates/switchyard-core/tests/real_gemini.rs` (opt-in). Every fact below was observed on
2026-11-22 against **Gemini CLI 0.70.0** (`@google/gemini-cli`, installed in a scratch directory)
with `gemini --acp`, a throwaway Git repository, the Gemini API key login and the
`gemini-3.5-flash-lite` / `gemini-3-flash-preview` models, using a logging ACP client. Claims not
marked *verified* are open questions.

## Summary

Gemini CLI can meet Switchyard's rule that **every tool call reaches Core before it runs**, but
only with a Switchyard-owned admin-tier policy of an exact shape, and with checks that the policy
loaded. Without that policy it runs reads and "known safe" shell commands (`ls`, `pwd`, `echo`)
without asking.

## Verified

### Login and access

- The free "Log in with Google" (`oauth-personal`) route is **closed** for Gemini CLI: session
  creation fails with "This client is no longer supported for Gemini Code Assist for individuals …
  migrate to the Antigravity suite of products" (0.43.0 and 0.70.0, ACP and `-p`).
- `initialize` lists auth methods `oauth-personal`, `gemini-api-key`, `vertex-ai`, `gateway`.
  `authenticate {methodId: "gemini-api-key"}` returns `{}`, but **in ACP mode Gemini does not read
  `~/.gemini/.env`**: the key must be in the process environment (`GEMINI_API_KEY`), or
  `session/new` fails with "Gemini API key is missing or not configured".
- Free tier: Pro models have a quota of **0**; `gemini-3-flash-preview` ran out after about ten
  requests in a day; quota errors come back as JSON-RPC error `429` on `session/prompt` with
  Google's explanation and a retry time.

### Protocol (ACP v1)

- `initialize` → `protocolVersion: 1`, `agentInfo {name: "gemini-cli", version: "0.70.0"}`,
  `agentCapabilities {loadSession: true, promptCapabilities {image, audio, embeddedContext},
  mcpCapabilities {http, sse}}`.
- `session/new {cwd, mcpServers: []}` returns the session id, **modes** (`default`, `autoEdit`,
  `yolo`, `plan`; current `default`) and **models** (`availableModels` with `modelId`/`name`,
  `currentModelId: "auto"`). Switchyard can take model choices from here (never its own list).
  The model can also be set at launch with `-m`.
- `session/prompt` ends with `{stopReason: "end_turn" | "cancelled", usage {inputTokens,
  outputTokens, cachedReadTokens, thoughtTokens, totalTokens}}` plus `_meta.quota.model_usage`.
- Updates: `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`,
  `usage_update`, `available_commands_update`, and on load `user_message_chunk`.
- For a new file, Gemini's `write_file` sends a diff with `oldText: ""` (not `null`); the tool name
  is in the call id (`write_file__call_…`, `replace__call_…`), and Switchyard classifies by it.
- **Tool calls carry no raw input.** A `tool_call` / permission request has `toolCallId, title,
  kind, status, locations, content`. Kinds seen: `read`, `edit`, `execute`, `think`. For `execute`
  the **title is the command line**; for `edit` the `locations` and `content` (diff) say what
  changes; for `read` the title/locations name the file. Classification must use these fields.
- `session/request_permission` offers `allow_always:proceed_always`, `allow_once:proceed_once`,
  `reject_once:cancel`. Answering `reject_once` makes the tool call `failed` and nothing happens
  (no file written, no command run). Switchyard must only ever answer `allow_once` or reject.
- `session/cancel` during a turn ends it with `stopReason: "cancelled"`.
- `session/load {sessionId, cwd, mcpServers}` works in a new process: it replays the history as
  updates, and the agent remembers the earlier turns.

### Permissions: what asks

| Setup | read | edit | execute `ls`/`pwd`/`echo` | execute `touch` | internal `think` |
|---|---|---|---|---|---|
| Default mode, no policy | runs | **asks** | runs | asks | runs |
| Admin rule `toolName="*"`, `ask_user` | asks | asks | **runs** | asks | asks |
| Plus admin rule `run_shell_command`, `argsPattern=".*"`, `ask_user`, higher priority | asks | asks | **asks** | asks | asks |

- The bypass is in Gemini's policy engine (`applyShellHeuristics`): a command it judges "known
  safe" turns `ask_user` into `allow` — **unless the deciding rule has an `argsPattern`**. Equal
  priorities let the wildcard rule decide, so the shell rule must have the higher priority.
- Admin-tier policies come from `--admin-policy <dir>` (tier 5, above user 4, workspace 3,
  extension 2, default 1) or `GEMINI_CLI_SYSTEM_SETTINGS_PATH`'s directory.
- **An invalid policy file is reported on stderr ("[ADMIN] Policy file error …") and ignored; the
  session runs without it (fails open).** Switchyard must detect this and refuse.
- A hostile repository (`.gemini/policies/*.toml` allowing everything, `.gemini/settings.json`
  asking for yolo) did **not** change anything: the command still asked and the reject stopped it.
  The repository is untrusted: Gemini also skips its project agents and hooks.
- `git` commands ask in an untrusted folder regardless (Gemini's own rule).

### The policy that worked

```toml
[[rule]]
toolName = "*"
decision = "ask_user"
priority = 900

# A pattern keeps Gemini's "known safe command" override from turning ask into allow.
[[rule]]
toolName = "run_shell_command"
argsPattern = ".*"
decision = "ask_user"
priority = 999
```

### Client file access (`fs.readTextFile` / `fs.writeTextFile`)

With the capabilities on, Gemini asked the client to read the file it was about to create; a
"not found" error made the edit fail **before** a permission request, and the model fell back to
`echo … > file` through the shell (which asked). Not usable as is; Switchyard will leave file
access to Gemini (after approval), like Claude Code and Codex.

## Open (Phase 0b or during Phase 1)

- Tool names and kinds of web search / web fetch / MCP calls; deny them in the admin policy and
  classify them as network (system deny) as a second line.
- Isolation: run with `GEMINI_CLI_HOME` (a Switchyard directory: no user policies, settings, MCP
  servers or extensions) and `GEMINI_CLI_TRUSTED_FOLDERS_PATH`; verify login still works with the
  key in the environment.
- `session/set_model` / `session/set_mode` behaviour; refuse `yolo`/`autoEdit` (never set them).
- Interrupting a turn while a permission request is pending; process exit and crash reporting.
- How the turn's final text and errors map to Switchyard's `AdapterOutput`; token usage and cost.
- Version gating: tested `0.70.0`; refuse untested versions unless allowed (as for Codex).
- The key is a secret: Switchyard passes `GEMINI_API_KEY` from its own environment to the
  process only, never logs it, never stores it in the journal.
