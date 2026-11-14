# Switchyard Core IPC protocol — version 4

This is the protocol between Switchyard Core and its clients (the desktop app, tests, and future
tools). The source of truth is `crates/switchyard-protocol`, and the TypeScript mirror is
`apps/desktop/src/types/protocol.ts`. This document and those two files must stay in sync.

## Transport

| Property    | Value |
|-------------|-------|
| Socket      | Unix domain stream socket |
| Location    | `$SWITCHYARD_SOCKET`, otherwise `<data dir>/core.sock` |
| Data dir    | `$SWITCHYARD_DATA_DIR`, otherwise `~/Library/Application Support/Switchyard` (macOS) or `$XDG_DATA_HOME/switchyard` / `~/.local/share/switchyard` |
| Access      | The socket is mode `0600` inside a `0700` data dir. Core also rejects peers whose UID differs from the socket owner (`getpeereid`). |
| Framing     | Newline-delimited JSON (JSON Lines): one UTF-8 JSON object per line, terminated by `\n`. `\r\n` is accepted and blank lines are ignored. |
| Frame limit | 1 MiB (`DEFAULT_MAX_FRAME_BYTES`). An oversized frame gets a `frame_too_large` error and the connection is closed, because the stream cannot be resynchronised. |

Socket paths are limited to 103 bytes (the `sun_path` limit on macOS). Core refuses to start with a
longer path; set `SWITCHYARD_SOCKET` to a shorter one if needed.

## Frames

Every frame has a `type` discriminator.

### Client → Core

```json
{"type":"request","id":1,"method":"system.health","params":{}}
```

- `id` is a client-chosen unsigned integer that is echoed in the reply. It should be unique among the
  client's in-flight requests.
- `params` may be omitted. Omitted params mean `{}`.

### Core → client

| `type` | Shape | Meaning |
|--------|-------|---------|
| `response` | `{"type":"response","id":1,"result":{…}}` | Request `id` succeeded. |
| `error` | `{"type":"error","id":1,"error":{"code":"not_found","message":"…","data":{…}}}` | Request `id` failed. `id` is `null` when the failing frame could not be attributed to a request (for example, invalid JSON). `data` is optional. |
| `event` | `{"type":"event","subscription_id":"…","event":{EventEnvelope}}` | An event on an active subscription. |
| `subscription_closed` | `{"type":"subscription_closed","subscription_id":"…","error":{…}}` | Core ended a subscription on its own. `error.data.last_seq` tells the client where to resume. |
| `live` | `{"type":"live","subscription_id":"…","output":{"session_id":"…","seq":42,"kind":"text","text":"Hel"}}` | Ephemeral agent output, sent only on `live.subscribe` subscriptions. `kind` is `text`, `thinking`, `message_start`, `message_stop` or `gap` (which carries `missed: n`). Not journaled, and lossy for slow clients. |
| `shutdown` | `{"type":"shutdown","reason":"SIGTERM"}` | Core is stopping. The connection closes after this frame. |

Requests on one connection run concurrently, so responses can arrive in a different order from the
requests. Clients correlate by `id`.

## Connection lifecycle

1. Connect to the socket.
2. Send `initialize`. Every other method fails with `not_initialized` until `initialize` succeeds. A
   second `initialize` fails with `already_initialized`.
3. Make requests and receive events.
4. Either side may close the connection at any time. Closing the connection ends all of its
   subscriptions and cancels its in-flight requests. **It never affects Core itself or anything Core
   manages.**

### Version negotiation

The client sends the highest protocol version it speaks. Core answers with
`min(client_version, PROTOCOL_VERSION)` if that is at least `MIN_SUPPORTED_PROTOCOL_VERSION`.
Otherwise it fails with `unsupported_protocol_version`, and `data` contains
`{min_supported, max_supported}`. A client must reject an answer higher than it requested.

Additive optional fields do not bump the version; any incompatible change does.

Versions:
- **Version 1:** milestones 1–2.
- **Version 2:** milestone 3; `sessions.start` requires `workspace_id`
  ([ADR 0015](../decisions/0015-protocol-version-2.md)).
- **Version 3:** milestone 4 approval policies
  ([ADR 0016](../decisions/0016-approval-policy-model.md)). It is incompatible with version 2:
  - a permission request may be allowed or denied by policy without creating an approval;
  - `Approval.decision_source` gains `policy`;
  - `approvals.decide {approve}` can fail with `policy_denied`;
  - `policies.*` methods are added.
- **Version 4 (current):** milestone 5 Codex
  ([ADR 0022](../decisions/0022-agent-capabilities-and-protocol-4.md)). It is incompatible with
  version 3:
  - every capability is `"supported" | "unsupported" | "unknown"` instead of a boolean, and
    capabilities `tool_activity`, `denial_reason_to_agent`, `usage_tokens`, `cost` and
    `file_read_tool` are added;
  - `sessions.start` accepts `provider: "codex"`, and `agents.list` returns one entry per provider;
  - additive: `Approval.provider`, and `usage` on `agent.turn_completed`.

The minimum is 4 on both sides:
- **A client that offers 1, 2 or 3** is refused with `unsupported_protocol_version`. `data` contains
  `{min_supported, max_supported, client_protocol, client_name, client_version, core_version,
  action: "update_client"}`. Its later requests get `not_initialized`.
- **A Core that selects 1, 2 or 3** (an older Core still running) is refused by the client.
  `switchyard-client` reports `IncompatibleCore {core_protocol, required, core_version, core_pid,
  client_version}`, closes the connection and sends nothing else. The desktop shows that message
  and does not stop the old Core by itself.

### Reconnection

The protocol is stateless across connections. To reconnect, a client opens a new connection and
calls `initialize` again. It then resubscribes with `after_seq` set to the last `seq` it processed.
Core replays the missed events from the journal before continuing live, so no event is lost and none
is duplicated. `core.instance_id` (from `initialize`) changes whenever Core restarts, which lets a
client detect a restart. `seq` stays continuous across restarts because it lives in the database.

## Methods

| Method | Params | Result |
|--------|--------|--------|
| `initialize` | `{protocol_version: u32, client: {name, version}}` | `{protocol_version, core: CoreInfo}` |
| `system.health` | — | `HealthReport` |
| `system.shutdown` | `{reason?: string}` | `{accepted: true}`. Core then shuts down gracefully. |
| `projects.list` | — | `{projects: Project[]}`, sorted by name |
| `projects.get` | `{project_id}` | `{project: Project}` |
| `projects.add` | `{path: string}` (absolute) | `{project: Project}` |
| `projects.remove` | `{project_id}` | `{project_id}`. Removes the registration only; files are untouched. `conflict` while a session is attached or while any worktree workspace is not `removed`. |
| `projects.git_status` | `{project_id}` | `GitStatusReport` |
| `events.list` | `{after_seq?, project_id?, session_id?, task_id?, event_types?: string[], limit?: 1..=1000 (default 100)}` | `{events: EventEnvelope[]}` in ascending `seq` order. Without any filter it returns the **latest** `limit` events; with filters it returns the first `limit` events after `after_seq`. |
| `events.subscribe` | `{after_seq?, project_id?, event_types?}` | `{subscription_id}` |
| `events.unsubscribe` | `{subscription_id}` | `{}` |
| `agents.list` | `{refresh?: bool}` | `{agents: [AgentAvailability]}` One entry per provider (`claude_code`, `codex`): installed, version, tested version, executable, error, auth `{logged_in, method, provider}` (never account identity), capabilities |
| `workspaces.list` | `{project_id, include_archived?: bool}` | `{workspaces: [Workspace]}`, oldest first. Archived, removed and legacy records only with `include_archived`. `project_not_found`. |
| `workspaces.get` | `{workspace_id}` | `{workspace, sessions: [Session]}` (the sessions that ran in it, newest first, up to 50) |
| `workspaces.create` | `{project_id, name?, base_ref?}` | `{workspace}` in `ready`/`dirty`. Creates `switchyard/<id>` from `base_ref` (default `HEAD`) in `<data>/workspaces/<project>/<id>`. Errors: `project_not_found`, `invalid_params` (name, unknown or option-like revision, no commits), `repository_unavailable`, `branch_collision`, `worktree_creation_failed`, `worktree_identity_mismatch`; on failure after the record was written, `data.workspace_id` names the `failed` record. |
| `workspaces.git_status` | `{workspace_id}` | `WorkspaceGitStatus`; records the observation, and `ready`/`dirty` follows it. `worktree_missing`, `legacy_workspace`. |
| `workspaces.verify` | `{workspace_id}` | `{workspace}`. Re-validates; `failed` returns to `ready`/`dirty` when valid, a settled workspace becomes `failed` when not. |
| `workspaces.archive` | `{workspace_id}` | `{workspace}`. Status only; files are kept. `invalid_workspace_state` when `active`, `provisioning`, …. |
| `workspaces.restore` | `{workspace_id}` | `{workspace}`. `archived` → `ready`/`dirty` after validation. |
| `workspaces.complete` | `{workspace_id}` | `{workspace}`. `ready`/`dirty` → `completed` (marker; can be reopened by starting a session). |
| `workspaces.cleanup_check` | `{workspace_id}` | `{workspace, report: CleanupReport}`. Read-only safety evaluation. |
| `workspaces.cleanup` | `{workspace_id, confirmation}` | `{workspace, report}` with status `removed`. The confirmation is bound to a full scan of every entry under the worktree, so any change after review refuses it; the scan is repeated immediately before `git worktree remove`. Refused with `dirty_workspace` (uncommitted or untracked changes) or `unsafe_cleanup` (any other blocker, a changed state, a change detected by the final scan, or a Git failure; the workspace is `archived` again and nothing is deleted). `data.report` holds the current report when available. The branch is always kept. |
| `sessions.start` | `{workspace_id, project_id?, prompt?, model?, provider?: "claude_code" \| "codex"}` | `{session: Session}`. `provider` defaults to `claude_code`; another value is `invalid_params`. Launches the agent with the workspace path as its working directory, never the project root. Errors: `workspace_not_found`, `workspace_project_mismatch`, `legacy_workspace`, `workspace_occupied`, `invalid_workspace_state`, `worktree_missing`, `worktree_identity_mismatch`, `repository_unavailable`, `agent_unavailable`, `agent_failed` (with `data.session_id`). |
| `sessions.list` | `{project_id?, workspace_id?, live_only?, limit?}` | `{sessions: [Session]}`, newest first |
| `sessions.get` | `{session_id}` | `{session, live_text, live_seq}`. `live_text` is the message currently streaming (ephemeral); it is empty between `message_stop` and the next `message_start`, because the complete message is journaled before `message_stop` (for example while a permission request waits). |
| `sessions.send` | `{session_id, text, queue_if_busy?: bool}` | `{delivery: "started_turn" \| "queued_for_next_turn"}`. Without `queue_if_busy`, sending during a turn fails with `invalid_state`. |
| `sessions.interrupt` | `{session_id}` | `{session}`. The state becomes `interrupted` when the agent confirms. |
| `sessions.stop` | `{session_id}` | `{session}` once the process has exited (state `stopped`) |
| `sessions.resume` | `{session_id, prompt?}` | `{session}`. Continues a terminal session in a new process with the same native ID (Claude `--resume`, Codex `thread/resume`; a failed Codex resume fails the session and never opens a new thread), in the same workspace, after the same validation as `sessions.start`. `conflict` if it is already attached; `legacy_workspace` for sessions from before workspace isolation. |
| `approvals.list` | `{status?, session_id?, project_id?, limit?}` | `{approvals: [Approval]}`, newest first |
| `approvals.decide` | `{approval_id, decision: "approve" \| "deny", note?}` | `{approval}` after the decision is durable. `conflict` if it is no longer pending. `note` is shown to the agent on deny. An approval is re-checked against the current policy in the same transaction: if the policy now blocks the request, it is settled `denied` (source `policy`), the agent is told, and the call fails with `policy_denied`. A deny is always accepted. |
| `policies.list` | `{project_id, include_archived?}` | `{rules: [PolicyRule], policy_revision}`: project and workspace rules of the project, oldest first |
| `policies.get` | `{rule_id}` | `{rule, history: [PolicyRuleRevision]}`: every revision, oldest first. `policy_rule_not_found`. |
| `policies.create` | `{project_id, workspace_id?, category, decision: "allow"\|"ask"\|"deny", conditions?: {path_prefix?, command?}, description, enabled?}` | `{rule, policy_revision}`. Errors: `project_not_found`, `workspace_not_found`, `workspace_project_mismatch`, `invalid_policy_scope` (legacy or removed workspace), `invalid_rule`, `unsupported_operation` (for example `allow` for shell without an exact command, or for network), `policy_conflict` (an active rule with the same scope, category and conditions; `data.rule_id`). Unknown condition fields are `invalid_params`. |
| `policies.update` | `{rule_id, expected_revision, decision?, conditions?, description?}` | `{rule, policy_revision}`. Scope and category cannot change. `stale_revision` (`data.current_revision`) if the rule changed since it was read; `invalid_rule` for an archived rule. |
| `policies.set_enabled` | `{rule_id, expected_revision, enabled}` | `{rule, policy_revision}` |
| `policies.archive` | `{rule_id, expected_revision}` | `{rule, policy_revision}`. The rule stops applying; its history remains. |
| `policies.effective` | `{project_id, workspace_id?}` | `EffectivePolicy`: system restrictions, per-category defaults and capabilities, active project rules and (with a workspace) that workspace's rules |
| `policies.evaluations` | `{project_id?, workspace_id?, session_id?, rule_id?, decision?, limit?}` | `{evaluations: [PolicyEvaluation]}`, newest first. `rule_id` matches evaluations the rule decided or matched. |
| `tasks.list` | `{project_id?, include_closed?, limit?}` | `{tasks: [Task]}`, newest first; completed and cancelled tasks only with `include_closed` |
| `tasks.get` | `{task_id}` | `TaskDetail`: `{task, dependencies: [TaskRef], dependents: [TaskRef], readiness: satisfied\|waiting\|broken, assignments, messages, findings, proposals}` |
| `tasks.create` | `{project_id, title, description?, kind?, priority?, acceptance_criteria?, depends_on?: [task_id], parent_task_id?, publish?}` | `{task}`: a `draft`, or with `publish` `ready`/`blocked`. Dependencies must be in the project; `invalid_params` for a cycle. `project_not_found`. |
| `tasks.update` | `{task_id, expected_revision, title?, description?, kind?, priority?, acceptance_criteria?}` | `{task}`. `conflict` on a stale revision; `invalid_state` while an agent works on it or once it is closed. |
| `tasks.publish` | `{task_id}` | `{task}`: `draft` → `ready` (or `blocked` while dependencies are open) |
| `tasks.add_dependency` / `tasks.remove_dependency` | `{task_id, depends_on}` | `{task}`. Only before work starts; cycles are refused (`invalid_params`). |
| `tasks.assign` | `{task_id, role: investigate\|implement\|review, provider, workspace_id, start?: bool (default true)}` | `{task, assignment}`. `review` needs `awaiting_review`; the others `ready` or `awaiting_review`. `agent_unavailable`; `unsupported` (no permission routing); `conflict` (open assignment, or the workspace is in use by another task's assignment, `data.task_id`). With `start`, as `tasks.start_assignment`. |
| `tasks.start_assignment` | `{assignment_id}` | `{task, assignment}`: starts (or resumes the task's earlier session for the same role, provider and workspace) a session (`access: review` for reviews), records the context, then sends it. It waits for Core's own stop of a session that just reported in that workspace; any other live session there is never stopped and the start fails (`workspace_occupied`). On failure the assignment fails and the task returns. |
| `tasks.cancel_assignment` | `{assignment_id, reason?}` | `{task, assignment}`; stops its session; pending approvals are cancelled with it |
| `tasks.note` | `{task_id, text}` | `{message, delivered}`: sent to the working agent now, otherwise kept for the next assignment |
| `tasks.complete` | `{task_id, note?}` | `{task}`: `awaiting_review` → `completed`, operator only; the last report becomes `result` |
| `tasks.fail` / `tasks.cancel` | `{task_id, note?}` | `{task}`; an open assignment is cancelled and its session stopped |
| `tasks.retry` | `{task_id, note?}` | `{task}`: `failed` → `ready`/`blocked`, or `blocked` → `ready` once dependencies are completed |
| `tasks.decide_proposal` | `{proposal_id, approve, note?}` | `{proposal, created_task}`: approving creates a draft subtask; once only (`conflict`) |
| `integrations.list` | `{project_id?, task_id?, include_closed?, limit?}` | `{integrations: [Integration]}`, newest first |
| `integrations.get` | `{integration_id}` | `IntegrationDetail`: `{integration, runs: [ValidationRun] (newest first), plan: [{command_id, name, command_line, policy_decision: allow\|ask\|deny, policy_explanation}]}` |
| `integrations.create` | `{task_id?, workspace_id?, target_branch?}` | `{integration}` bound to the workspace branch tip and the target tip (default target: the branch checked out in the project checkout). `invalid_params` for another project, a `switchyard/*` or option-like target, a missing branch; `conflict` if the workspace has an unfinished integration. |
| `integrations.refresh` | `{integration_id}` | `{integration}` re-read from Git and rebound to the current commits; any approver is cleared |
| `integrations.changes` | `{integration_id}` | `IntegrationChanges` for the bound commits: commits `target..source`, files from the merge base (renames, binaries, counts), `diverged`, and the workspace's uncommitted files |
| `integrations.patch` | `{integration_id, path?, uncommitted?}` | `{patch, truncated}` (cut at 1 MiB) |
| `integrations.commit_workspace` | `{integration_id, message}` | `{integration}`; commits everything in the workspace (completes a resolution; refused while conflict markers remain) |
| `integrations.update_from_target` | `{integration_id}` | `{integration}`; merges the target into the workspace; conflicts → `resolving`. The target never changes. |
| `integrations.abort_resolution` | `{integration_id}` | `{integration}`; `git merge --abort` in the workspace |
| `integrations.validate` | `{integration_id, authorize?: [{command_id, command_line}]}` | `{integration, run}`; an authorization covers a command only while its `command_line` is the one shown in the plan. Refused commands (policy `deny`, or `ask` not authorized) mean nothing runs; otherwise runs in the background (`validating`) |
| `integrations.cancel_validation` | `{integration_id}` | `{integration}`; stops the running commands |
| `integrations.approve` | `{integration_id, source_commit, target_commit}` | `{integration}` (`integrated` or `failed`). `conflict` if the commits are not the bound ones or moved (the integration is re-prepared), or it is already integrated; `invalid_state` unless `ready` |
| `integrations.cancel` | `{integration_id, reason?}` | `{integration}` (`cancelled`); nothing in Git changes |
| `integrations.settings` / `integrations.set_settings` | `{project_id}` / `{project_id, require_review}` | `{project_id, require_review, revision, commands: [ValidationCommand]}` |
| `integrations.set_command` | `{project_id, command_id?, name, argv: [string], timeout_secs (1..=14400), enabled?, position?}` | settings; argv is run directly, never through a shell |
| `integrations.delete_command` | `{command_id}` | settings |
| `live.subscribe` | `{session_id?}` | `{subscription_id}`. Delivers `live` frames. |
| `live.unsubscribe` | `{subscription_id}` | `{}` |

Unknown fields in method params are rejected with `invalid_params`.

### `events.subscribe` semantics

- Core always sends the `response` before any `event` frame for that subscription.
- With `after_seq`, stored events with `seq > after_seq` are replayed, then live events follow. There
  are no gaps or duplicates.
- Without `after_seq`, only events published after the subscription are delivered.
- Delivery is ordered by `seq` and lossless. If a client reads slowly, Core's per-connection queue
  fills and the subscription falls behind the live channel. Core then catches up from the journal
  instead of dropping events (see ADR 0004).
- Each connection can hold at most 16 subscriptions (`overloaded` beyond that).

### Data types

```text
CoreInfo         { version, instance_id: uuid, started_at_ms, pid }
HealthReport     { status: "ok"|"degraded", core: CoreInfo, protocol_version, uptime_ms,
                   database: { initialized, path, journal_mode?, schema_version?, error? },
                   projects_count?, active_sessions_count?, last_event_seq?, connected_clients }
Project          { id, name, root_path, created_at_ms, updated_at_ms, accessible }
GitStatusReport  { project_id, branch?, head_oid?, detached, upstream?, ahead?, behind?,
                   staged, unstaged, untracked, conflicted, clean, checked_at_ms }
Workspace        { id, project_id, kind: "worktree"|"legacy_checkout", name, path, branch?, base_ref?,
                   base_commit?, status, status_reason?, dirty?, git_checked_at_ms?, created_at_ms,
                   updated_at_ms, last_activity_at_ms, archived_at_ms?, active_session_id?, path_exists }
WorkspaceGitStatus { workspace_id, branch?, expected_branch?, on_expected_branch, head_oid?, detached,
                   base_commit?, commits_since_base?, staged, unstaged, untracked, conflicted, clean,
                   checked_at_ms }
Session / Approval  see crates/switchyard-protocol/src/model.rs; both carry workspace_id and
                   workspace_name (additive, milestone 3) so clients can label them without lookups.
                   Approval.policy (v3): { evaluation_id, category, reason, explanation,
                   deciding_rule?: PolicyRuleRef, policy_revision }; null for approvals recorded
                   before policies. decision_source adds "policy" (always a denial).
                   Approval.provider (v4): the session's agent.
                   Approval.policy.scope (v4, additive): { kind: "workspace"|"outside_workspace"|
                   "unknown"|"unrestricted", outside_paths, unresolved_paths, note }: what approving
                   would reach, established by Core before asking. Shell commands are always
                   "unrestricted".
                   Session.capabilities / AgentAvailability.capabilities (v4): every field is
                   "supported" | "unsupported" | "unknown".
                   Session.access (v4, additive, M6): "write" | "review". Review sessions cannot
                   change files (ADR 0027); absent in older payloads means "write".
Task             { id, project_id, parent_task_id?, title, description?, kind: investigation|
                   implementation|general, priority: low|normal|high|urgent, acceptance_criteria,
                   status: draft|ready|blocked|assigned|in_progress|awaiting_review|in_review|
                   completed|failed|cancelled, status_reason?, result?, failure?, origin:
                   operator|proposal, origin_proposal_id?, revision, created_at_ms, updated_at_ms,
                   completed_at_ms?, open_assignment?: TaskAssignment, dependency_ids,
                   pending_proposals, open_findings }
TaskAssignment   { id, task_id, role: investigate|implement|review, provider, workspace_id,
                   session_id?, status: pending|active|reported|failed|cancelled, context?
                   (the TaskContext exactly as sent), context_version?, outcome?, created_at_ms,
                   started_at_ms?, ended_at_ms? }
TaskMessage      { id, task_id, assignment_id?, kind, sender: agent|operator|core,
                   sender_session_id?, recipient: operator|agent|task, payload, causation_id?,
                   delivery: recorded|pending|delivered, delivered_at_ms?, created_at_ms }
                   kind: task_assignment | status_update | info_request | findings | proposal |
                   review_result | blocker | delegation_request | completion_report |
                   operator_note | malformed_report
TaskFinding      { id, task_id, message_id, assignment_id?, severity: info|low|medium|high|critical,
                   title, explanation?, suggestion?, path?, line?, status: open|resolved|dismissed,
                   created_at_ms, resolved_at_ms? }
Integration      { id, project_id, task_id?, workspace_id, workspace_name?, source_branch, source_commit,
                   target_branch, target_commit, base_commit?, status: open|conflicted|resolving|
                   validating|ready|integrating|integrated|failed|cancelled, status_reason?,
                   blockers: [{code, message}], preflight: {facts, blockers, summary, require_review},
                   prepared_at_ms, approved_by?, approved_at_ms?, merge_commit?, method?, failure?,
                   revision, created_at_ms, updated_at_ms, completed_at_ms? }
ValidationRun    { id, source_commit, status: running|passed|failed|cancelled|interrupted,
                   requested_by?, started_at_ms, ended_at_ms?, steps: [{ id, position, name,
                   command_line, cwd, timeout_secs, policy_decision, policy_explanation,
                   authorized_by?, status: pending|running|passed|failed|timed_out|cancelled|refused|
                   skipped|error, exit_code?, signal?, stdout?, stderr?, stdout_truncated,
                   stderr_truncated, detail?, started_at_ms?, ended_at_ms? }] }
TaskProposal     { id, task_id, message_id, position, proposal: {title, description?, kind,
                   acceptance_criteria, depends_on: [index|task_id]}, status: pending|approved|
                   rejected, decided_by?, decision_note?, created_task_id?, created_at_ms,
                   decided_at_ms? }
PolicyRule       { id, project_id, workspace_id?, workspace_name?, scope: "project"|"workspace",
                   category, decision, conditions: {path_prefix?, command?}, description, enabled,
                   archived, revision, created_at_ms, updated_at_ms, archived_at_ms? }
PolicyRuleRevision { revision, policy_revision, change: "created"|"updated"|"enabled"|"disabled"|
                   "archived", rule: PolicyRule (as it was), changed_at_ms, changed_by? }
PolicyRuleRef    { id, revision, scope, workspace_id?, decision, description }  (as evaluated)
EffectivePolicy  { project_id, workspace_id?, policy_revision, system_version,
                   system_restrictions: [{code, message}], categories: [{category,
                   default_decision, configurable, allow_supported, path_condition,
                   command_condition}], project_rules: [PolicyRule], workspace_rules: [PolicyRule] }
PolicyEvaluation { id, evaluated_at_ms, trigger: "request"|"reevaluation"|"decision_check",
                   project_id, workspace_id?, workspace_name?, session_id, adapter, tool_name,
                   category, input: {operation, paths}, decision, reason: "system_restriction"|
                   "rule"|"default"|"capped", explanation, deciding_rule?, matched_rules,
                   restrictions: [{code, message}], policy_revision, system_version, approval_id?,
                   approval_status? }

category: file_read | file_write | file_edit | file_delete | directory_list | search | shell |
          network | external_tool | unknown
CleanupReport    { allowed, blockers: [{code, message}], warnings: [{code, message}],
                   will_remove_directory, branch, branch_kept: true, ignored_files: [string],
                   ignored_total, unmerged_commits?, merged_into?, entries_total?, bytes_total?,
                   tree_fingerprint?, confirmation }
EventEnvelope    { seq, id: uuid, schema_version, event_type, occurred_at_ms,
                   project_id?, workspace_id?, session_id?, task_id?, agent_id?, causation_id?,
                   payload: object }
```

The health figures come from a live database query on every call. If that query fails, the counts
are `null`, `database.error` is set, and `status` is `degraded`. They are never cached or estimated.

`Project.accessible` is computed when a project is read. If the directory is missing or unreadable,
the registration is kept and `projects.git_status` returns `repository_unavailable`.

### Event types (schema version 1)

| `event_type` | Payload |
|--------------|---------|
| `core.started` | `{core_version, instance_id}` |
| `core.stopping` | `{instance_id, reason}` |
| `project.registered` | `{project_id, name, root_path}` |
| `project.removed` | `{project_id, root_path}` |
| `agent.started` | `{provider, cli_version, pid, cwd, native_session_id, resumed}` |
| `agent.ready` | `{native_session_id, model, tools}` |
| `agent.state_changed` | `{from, to}` |
| `agent.turn_started` | `{prompt, prompt_truncated, queued}` |
| `agent.message` | `{message_id, text, truncated}` (complete messages only) |
| `agent.tool_started` | `{tool_use_id, tool_name, input, input_truncated}` |
| `agent.tool_completed` / `agent.tool_failed` | `{tool_use_id, tool_name, output, output_truncated[, rejected]}`; `causation_id` is the matching `tool_started` |
| `agent.turn_completed` | `{success, interrupted, reason, stop_reason, duration_ms, cost_usd, turn_cost_usd?, usage?, result}`. `cost_usd` is the session's running total as reported by the agent, not this turn's cost; `turn_cost_usd` is this turn's cost, omitted when unreported (aborted turns, and events journaled before it was added). `usage` (`input_tokens`, `cached_input_tokens`, `output_tokens`, `reasoning_output_tokens`, `total_tokens`) is this turn's token counts, omitted when unreported (Claude, aborted turns). |
| `agent.permission_evaluated` | `{evaluation_id, trigger, tool_name, category, decision, reason, explanation, rule_id, rule_revision, policy_revision, approval_id, target}`: every evaluation. For `allow`/`deny` it is the final, automatic decision; for `ask` an `agent.permission_requested` follows. `target` is the path or command (at most 512 bytes). |
| `agent.permission_requested` | `{approval_id, tool_name, tool_use_id, input, description, expires_at_ms, evaluation_id, explanation}` (only for `ask`) |
| `agent.permission_resolved` | `{approval_id, tool_name, status, source, decided_by, note}`; `source` `policy` means a policy change denied the pending request (`note` explains) |
| `policy.rule_changed` | `{rule_id, change, revision, policy_revision, scope, category, decision, enabled, archived, description, conditions}`; context carries `project_id` (and `workspace_id` for workspace rules) |
| `agent.interrupted`, `agent.session_resumed`, `agent.warning`, `agent.exited`, `agent.failed` | See `crates/switchyard-events/src/envelope.rs` |
| `workspace.created` | `{name, path, branch, base_ref, base_commit}` (status `provisioning`) |
| `workspace.state_changed` | `{from, to, reason}` |
| `workspace.cleanup_requested` | `{allowed, blockers: [code], warnings: [code], will_remove_directory, tree_fingerprint, entries}` |
| `workspace.removed` | `{path, branch, directory_removed, branch_kept: true}` |
| `workspace.cleanup_failed` | `{reason, detail}`; the workspace was preserved |
| `task.created` | `{title, kind, priority, status, origin: operator\|proposal, parent_task_id, origin_proposal_id}` |
| `task.updated` | `{revision, fields: [string]}` |
| `task.status_changed` | `{from, to, reason}` |
| `task.dependency_added` / `task.dependency_removed` | `{depends_on}` |
| `task.assigned` | `{assignment_id, role, provider, workspace_id}` |
| `task.assignment_started` | `{assignment_id, session_id, access: write\|review, context_version, resumed}` |
| `task.assignment_ended` | `{assignment_id, status: reported\|failed\|cancelled, outcome}` |
| `task.message_recorded` | `{message_id, kind, sender: agent\|operator\|core, assignment_id, summary}`; a message from an agent never authorizes anything |
| `task.review_recorded` | `{assignment_id, reported_result, result, workspace_changed, findings}`; `result` is `blocked` when the workspace changed during the review |
| `task.proposal_decided` | `{proposal_id, decision: approved\|rejected, created_task_id}` |
| `integration.created` | `{integration_id, source_branch, target_branch, created_by}` |
| `integration.prepared` | `{integration_id, source_commit, target_commit, base_commit, blockers: [code], files_changed}` |
| `integration.status_changed` | `{integration_id, from, to, reason}` |
| `integration.workspace_changed` | `{integration_id, action: committed\|updated\|conflicts\|aborted, commit, files, by}` |
| `integration.validation_started` | `{integration_id, run_id, source_commit, commands}` |
| `integration.validation_step_finished` | `{integration_id, run_id, name, command, status, exit_code, authorized_by, policy_decision}` |
| `integration.validation_finished` | `{integration_id, run_id, source_commit, status}` |
| `integration.approved` | `{integration_id, source_commit, target_commit, merge_commit, method: ref_update\|checkout_fast_forward, approved_by}`; recorded before the target moves |
| `integration.integrated` | `{integration_id, target_branch, merge_commit, source_commit, target_commit, recovered}` |
| `integration.failed` | `{integration_id, reason, target_moved}` |
| `files.changed` | `{changes: [{path, kind: created\|modified\|deleted\|renamed, from?}], dropped}`: what the filesystem reported in one workspace (debounced, `.git` and ignored paths excluded). Paths are relative to the workspace root; the context carries project, workspace and session. |

Agent events carry `project_id`, `workspace_id`, `session_id` and `agent_id` in the envelope; workspace events carry `project_id` and `workspace_id`; task events carry `project_id` and `task_id` (and `workspace_id` for assignment events); integration events carry `project_id`, `workspace_id` and (when linked) `task_id`, with `integration_id` in the payload. `events.list` also accepts `session_id` and `task_id`.

Clients must tolerate unknown event types and payload fields.

## Error codes

| Code | When |
|------|------|
| `parse_error` | Invalid JSON or an unknown frame shape. The connection stays open. |
| `frame_too_large` | The frame exceeds the limit. The connection is closed. |
| `not_initialized`, `already_initialized`, `unsupported_protocol_version` | Handshake errors. |
| `method_not_found`, `invalid_params` | Request errors. |
| `not_found`, `conflict` | The entity does not exist, or already exists (for example, a repository is already registered). |
| `invalid_path` | The path is not absolute, does not exist, or is not a directory. |
| `permission_denied` | Core cannot read the path. |
| `not_a_git_repository` | The path is not a Git working tree, is a bare repository, or is a subdirectory of a repository (`data.repository_root` is given in that last case). |
| `repository_unavailable` | A registered repository is gone or unreadable. |
| `git_unavailable`, `git_failed` | `git` cannot be executed, or it failed or timed out (15 s). |
| `agent_unavailable` | The Claude Code CLI is missing, unusable or lacks required flags. |
| `invalid_state` | The operation does not fit the session state (for example, sending during a turn without `queue_if_busy`, or interrupting an idle session). |
| `unsupported` | The agent or provider cannot do this. |
| `agent_failed` | The agent process failed to start or to accept a command. |
| `project_not_found`, `workspace_not_found` | The id names no project / workspace (workspace and session methods). |
| `workspace_project_mismatch` | The workspace belongs to another project. |
| `invalid_workspace_state` | The operation does not fit the workspace status (`data.status`, `data.reason`). |
| `workspace_occupied` | Another session is attached to the workspace. |
| `legacy_workspace` | A pre-isolation record of the project checkout (no sessions, no resume, no cleanup). |
| `worktree_creation_failed`, `branch_collision` | `git worktree add` failed / the branch already exists (never reused). |
| `worktree_identity_mismatch` | Wrong path, not a worktree, another repository, or another branch checked out (`data.reason`). |
| `worktree_missing` | The workspace directory does not exist. |
| `dirty_workspace`, `unsafe_cleanup` | Cleanup refused (see `workspaces.cleanup`). |
| `policy_rule_not_found` | The rule id names no rule. |
| `invalid_rule` | The rule is malformed (bad path or command condition, condition not applicable to the category, missing description) or archived. There is no separate "policy" entity: a project's or workspace's policy is its set of rules. |
| `invalid_policy_scope` | The workspace cannot carry rules (legacy checkout or removed). |
| `unsupported_operation` | The category/decision combination is not supported (any rule for `unknown`; `allow` for network or external tools; `allow` for shell without an exact command). |
| `policy_conflict` | An active rule with the same scope, category and conditions exists. |
| `stale_revision` | `expected_revision` is not the rule's current revision. |
| `policy_evaluation_failed` | Reserved: a request could not be evaluated. Agents are denied in that case; no client call returns it today. |
| `policy_denied` | The current policy blocks the request, so it cannot be approved. |
| `shutting_down` | Core is stopping. |
| `overloaded` | More than 32 concurrent requests, or too many subscriptions, on one connection. |
| `internal` | An unexpected failure. Details are logged by Core. |

## Example session

```text
→ {"type":"request","id":1,"method":"initialize","params":{"protocol_version":4,"client":{"name":"cli","version":"0.1.0"}}}
← {"type":"response","id":1,"result":{"protocol_version":4,"core":{"version":"0.1.0","instance_id":"01a1…","started_at_ms":1794354584000,"pid":72726}}}
→ {"type":"request","id":2,"method":"events.subscribe","params":{"after_seq":0}}
← {"type":"response","id":2,"result":{"subscription_id":"01a1…"}}
← {"type":"event","subscription_id":"01a1…","event":{"seq":1,"event_type":"core.started",…}}
→ {"type":"request","id":3,"method":"projects.add","params":{"path":"/Users/me/src/api"}}
← {"type":"event","subscription_id":"01a1…","event":{"seq":2,"event_type":"project.registered",…}}
← {"type":"response","id":3,"result":{"project":{…}}}
```

## Tests

`crates/switchyard-core/tests/` holds the protocol tests:

- `ipc_protocol.rs`: raw wire behaviour (handshake, negotiation, errors, frame limits, health).
- `projects.rs`: project registration end to end.
- `events.rs`: delivery, replay, filters and shutdown notification.
- `policies.rs`: policy evaluation of every permission request, rule management, revisions,
  pending approvals and policy changes, failure handling.
- `process.rs`: the real binary, covering disconnect and reconnect, the single-instance lock,
  stale-socket recovery and signals.
