# Recorded Gemini CLI ACP sessions (Gemini CLI 0.70.0)

Real `gemini --acp` sessions recorded on 2026-11-22 (macOS, Gemini API key login,
`gemini-3.5-flash-lite`) in a throwaway repository, with a research client that sent
`initialize` (no file system, no terminal), `authenticate {methodId: "gemini-api-key"}`,
`session/new` (or `session/load`) and one `session/prompt`, and answered permission requests as
listed. Gemini ran with Switchyard's admin policy (`--admin-policy`, the policy in
`src/acp/launch.rs`). `*.stdout.jsonl` is what the agent printed.

Sanitised before committing: only agent → client lines are kept; the repository path is
`/workspace/demo`, other temporary paths `/workspace/…`, the user name `user`. The request ids
match the adapter's (initialize 1, authenticate 2, session 3, prompt 4). Every other line is
unmodified agent output.

| Fixture | Scenario |
|---|---|
| `ask_all` | `ls -la` asks (the shell rule with `argsPattern`), allowed once → runs |
| `reject` | a `write_file` and a `touch` both ask, both rejected → both tool calls `failed`, nothing written |
| `cancel` | `session/cancel` 4 s into a streaming essay → `stopReason: "cancelled"` |
| `load` | `session/load` of the `ask_all` session in a new process: the history replays, then a turn |
