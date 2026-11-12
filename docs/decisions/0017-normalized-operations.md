# ADR 0017: Normalized operation classification

- Status: accepted (2026-11-11)

## Context

Policies must not depend on Claude's JSON. Codex reports approvals differently (command executions,
file changes, MCP elicitations). A tool name also says little about the effect: Bash can read, write,
delete, reach the network and run anything.

## Decision

### Every tool goes through Switchyard

- The Claude launch settings now carry an `ask` rule for **every enabled tool**: Read, Write, Edit,
  Glob, Grep, Bash and NotebookEdit. Previously only Bash had one.
- Verified with 2.1.322: Read, Glob and Grep inside the working directory now arrive as
  `can_use_tool`. Before, Claude allowed them silently.
- So there is exactly one decision path: adapter request → classification → evaluation → allow, ask
  or deny. No Claude tool is decided outside Switchyard.
- Reads outside the working directory are still refused by Claude itself (`--restricted`) before
  they reach Switchyard. That refusal is stricter than any policy.

### Canonical operation (`switchyard_policy::Operation`)

| Field | Meaning |
|-------|---------|
| `adapter` | `claude_code` |
| `tool` | Original tool name, for display and audit only; rules never match on it |
| `category` | `file_read`, `file_write`, `file_edit`, `file_delete`, `directory_list`, `search`, `shell`, `network`, `external_tool` or `unknown` |
| `paths` | Target paths exactly as requested (file and search operations) |
| `command` | Shell command text (`shell`) |
| `background` | The shell command would keep running after the tool returns |
| `unexpected_fields` | Input fields the adapter does not know |
| `problem` | Why classification failed: malformed input, missing tool name, or a tool that is not enabled |

Project, workspace, session and request ids are attached by Core, never taken from the adapter's
payload.

### Claude classification (`switchyard_agents::claude::classify`)

The mapping lives inside the adapter, next to the wire parser. Core sees only `Operation`.

| Tool | Category | Targets | Known input fields |
|------|----------|---------|--------------------|
| Read | `file_read` | `file_path` | `file_path offset limit pages` |
| Write | `file_write` | `file_path` | `file_path content` |
| Edit | `file_edit` | `file_path` | `file_path old_string new_string replace_all` |
| NotebookEdit | `file_edit` | `notebook_path` | `notebook_path cell_id new_source cell_type edit_mode` |
| Glob | `search` | `path`, or the workspace when omitted; a pattern that is absolute or contains `..` counts as an unresolvable target | `pattern path` |
| Grep | `search` | `path`, or the workspace when omitted | `pattern path glob output_mode -A -B -C -n -i type head_limit offset multiline context` |
| Bash | `shell` | none (the command can touch anything) | `command description timeout run_in_background dangerouslyDisableSandbox` |
| `WebFetch`, `WebSearch` | `network` | – | – |
| `mcp__*` | `external_tool` | – | – |
| anything else | `unknown` | – | – |

**Malformed input becomes `unknown`** with a `problem`, and is denied. That covers:
- input that is not an object;
- a required field that is missing or not a string;
- an empty path;
- a NUL byte;
- a missing or empty tool name.

**Tools that are not enabled are also denied.** `network` and `external_tool` are classified so that
the audit shows what was asked. But a request for a tool outside the adapter's launch list (`--tools`)
is a system denial, whatever its category. Rules cannot enable a tool.

There are categories with no Claude tool:
- **`file_delete` and `directory_list`:** deletion and listing happen through Bash, which is `shell`.
  The categories exist for other adapters (for example Codex `fileChange` deletions).
- **Unchanged categorization:** Bash is always `shell`. Switchyard does not parse shell commands, so
  `ls` is not a "directory listing" and `rm` is not a "file deletion": the effects of a command
  cannot be known from its text.

### Path facts (Core, before evaluation)

Core resolves each target before the pure evaluation and stores the result with the operation:

- **Lexical location:** a relative target is joined to the workspace path. Then `.` and `..` are
  normalized, and the result is checked to lie under the workspace path.
- **Resolved location:** the nearest existing ancestor is canonicalized (`realpath`, which follows
  symlinks), the remaining components are appended, and the result is checked to lie under the
  canonical workspace path.
- **Outcomes:** `inside {lexical, resolved}` (both relative to the workspace), `outside` or
  `unresolved`. An `allow` needs `inside`; anything else is capped at `ask`.

These facts are captured once, at request time. Re-evaluation (ADR 0018) uses the stored facts and
does no new I/O.

### Stored operation

The evaluation record keeps the operation with:
- paths;
- a command bounded to 4 KiB;
- the field *names* only.

File contents (`content`, `old_string`, `new_string`, `new_source`) are **never** stored in policy
records. The approval request keeps the full input, as it did before (bounded to 256 KiB), so that
the operator can judge it.

## Consequences

- Every Read, Glob and Grep now costs a Core round trip, an evaluation row and a journal event.
  That is local and cheap, and it is the price of an audit trail and the ability to block reads.
- A new Claude input field turns automatic allows into asks until the adapter learns it. That fails
  closed, and it is visible in the explanation ("unexpected input fields").
- A new Claude tool is denied until it is classified and enabled.
