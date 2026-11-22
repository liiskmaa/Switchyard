# ADR 0044: Gemini CLI through the Agent Client Protocol

- Status: accepted (2026-11-22)
- Verified against Gemini CLI 0.70.0 (`docs/architecture/acp-integration.md`)

## Context

The operator wanted a third agent. Gemini CLI speaks ACP (JSON-RPC over stdio), an open standard
other agents share, so one adapter can serve several. Switchyard's rule is that every tool call
reaches Core's policy before it runs; Gemini, by default, runs reads and commands it judges safe
without asking, loads the repository's own policies and settings, and its personal Google login no
longer works for the CLI (API key or Vertex only).

## Decision

1. **Adapter** `switchyard_agents::acp`: a pure ACP v1 state machine (`protocol.rs`), Gemini's
   launch (`launch.rs`), classification (`classify.rs`), detection and the model list (read from
   `session/new`). Provider `gemini`; tested version 0.70.0, other versions refused unless
   `SWITCHYARD_ALLOW_UNTESTED_GEMINI=1`.
2. **Every action asks.** Gemini runs with `--admin-policy` (the highest policy tier, above the
   user's and the repository's) holding Switchyard's policy: every tool `ask_user`; the shell tool
   with an `argsPattern` and higher priority, without which Gemini turns "ask" into "allow" for
   commands it judges safe; web, sub-agents, MCP, skills, plan-mode switches and Gemini's own
   question tool denied; only `update_topic` and `write_todos` (no effect outside the
   conversation) allowed. A policy load error is printed and ignored by Gemini (fails open), so
   the session driver stops the session as soon as stderr reports one.
3. **Isolation.** `GEMINI_CLI_HOME` is Switchyard's own directory (`<data>/gemini/home`), so the
   user's Gemini settings, extensions and MCP servers never apply; no folder is trusted, so the
   repository's `.gemini` settings and hooks are ignored (verified with a hostile repository).
   The client offers no file system and no terminal capability; the session must open and stay in
   the `default` (ask) mode, or Switchyard stops it.
4. **Answers.** A permission is answered with the `allow_once` option or a rejection, never an
   "always" option; an interrupt sends `session/cancel` and answers pending requests as
   cancelled.
5. **Classification** from what ACP carries (no raw input): kind, title (the command line for
   `execute`), locations and diffs; Gemini's tool name from the call id (`write_file` → write,
   `replace` → edit); anything unclear is denied.
6. **Login.** The API key comes from `GEMINI_API_KEY` or, when the desktop app's environment has
   none, from `~/.gemini/.env`, passed to the Gemini process only and never logged or stored.
7. **Storage.** Migration 0014 rebuilds `agents`, `sessions` and `task_assignments` to accept
   `gemini`; the migration runner gained a `foreign_keys=off` marker for table rebuilds (and
   refuses to commit if `foreign_key_check` finds a broken reference).

## Consequences

- Teams consider Gemini once it is installed at the tested version.
- The free API tier is small (Pro models 0 requests); Flash-Lite works for trying it.
- Another ACP agent is a new launch profile plus its own verification, not a new protocol.
