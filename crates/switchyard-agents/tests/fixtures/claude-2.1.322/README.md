# Recorded Claude Code transcripts (CLI 2.1.322)

Real `claude -p` stream-json sessions recorded on 2026-11-11 with:

```
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-partial-messages --session-id <uuid> --model haiku \
  --permission-mode default --permission-prompt-tool stdio --restricted --tools default
```

`*.stdout.jsonl` is what the CLI printed and `*.stdin.jsonl` is what the driver sent.

The transcripts were sanitised before they were committed:

- Local paths are replaced with `/workspace/demo`.
- Thinking signatures are replaced with `"SIG"`.
- `rate_limit_event` details and `credential_expires_at` are removed.
- `stream_event` lines carrying `input_json_delta` are dropped. They fragment local paths, and
  Switchyard reads complete `tool_use` blocks instead.

Every other line is unmodified CLI output.

| Fixture | Scenario |
|---------|----------|
| `write_allow` | (second recording; `notes.txt` already existed, so Claude read it first) Write tool → `can_use_tool` → allow → file created; second turn in the same process |
| `write_deny` | Same request → deny → `tool_result` with `is_error: true`, the operator message and `permission_denials` |
| `queued_message` | Second user message sent 3 s into a running turn; it runs as the next turn |
| `interrupt` | `interrupt` control request mid-stream → `still_queued: []` → `result` `error_during_execution` / `aborted_streaming`; next turn works |
| `interrupt_pending_permission` | Interrupt while `can_use_tool` is unanswered → `control_cancel_request` → rejected `tool_result` → `aborted_tools` |
| `resume` | `--resume 79affdf0-a2dd-4729-8a50-88aef6a07705` (a first `write_allow` recording that was later re-recorded): the CLI keeps that native `session_id` and remembers that it created `notes.txt` |
| `bash_ask` | Final launch flags (`--strict-mcp-config`, explicit `--tools`, `--settings '{"permissions":{"ask":["Bash"]}}'`): even read-only `ls` and `cat /etc/hosts` reach `can_use_tool`; two parallel tool calls produce two pending requests |
