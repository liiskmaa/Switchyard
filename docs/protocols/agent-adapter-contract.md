# Agent adapter contract (draft v0)

Status: **specification for a future milestone. Nothing here is implemented yet.** It defines how
Switchyard Core will drive native coding agents through a common interface while keeping each
vendor's own capabilities. Background: [agent-integration.md](../architecture/agent-integration.md).

## Principles

1. **Native agents, not API calls.** An adapter supervises a vendor's own CLI process (`claude`,
   `codex`). It never reimplements the agent loop or calls model APIs directly.
2. **Core owns processes.** Adapters live inside Core (a future `switchyard-agents` crate) and spawn
   children of Core. The UI only observes and issues commands through the Core protocol.
3. **Common core, explicit extensions.** Behaviour that every adapter supports goes in the common
   interface. Anything vendor-specific is advertised through capabilities and exposed as typed
   extensions, never squeezed into a lowest-common-denominator shape.
4. **Everything important becomes an event.** Adapter output is normalized into Switchyard events,
   carrying `session_id`, `agent_id`, `workspace_id`, `project_id` and `task_id`, plus a
   `causation_id` that links back to the command that caused them.
5. **No silent permissions.** Every vendor approval request becomes a Switchyard approval. It is
   answered by central policy or by the operator, and the decision is journaled.

## Lifecycle

```text
             start()                 turn completes / idle
  ┌────────┐ ───────► ┌──────────┐ ─────────────────────► ┌──────┐
  │starting│          │  active  │ ◄───────────────────── │ idle │
  └────────┘          └──────────┘   send_input()/steer   └──────┘
      │ spawn/handshake    │ interrupt() ends turn only        │
      ▼ failure            ▼ process exit / stop()             ▼ stop()
  ┌──────┐            ┌─────────────────────────┐
  │failed│            │ completed | terminated  │
  └──────┘            └─────────────────────────┘
```

These states map 1:1 to `sessions.status` in the schema.

## Common interface (Rust sketch)

```rust
/// Static description of an installed agent.
pub struct AgentDescriptor {
    pub vendor: Vendor,                 // ClaudeCode | Codex
    pub executable: PathBuf,
    pub cli_version: String,            // detected at registration
    pub capabilities: Capabilities,
}

pub struct Capabilities {
    pub streaming_text: bool,           // token/message deltas
    pub steer_running_turn: bool,       // Codex turn/steer
    pub interrupt_turn: bool,           // Claude SIGINT/control request, Codex turn/interrupt
    pub resume_session: bool,
    pub fork_session: bool,
    pub approval_requests: ApprovalSupport, // which kinds are routed to Switchyard
    pub structured_output: bool,
    pub review_mode: bool,              // Codex review/start
    pub extensions: Vec<ExtensionId>,   // vendor-specific operations
}

pub struct SessionSpec {
    pub session_id: Uuid,               // Switchyard id (Claude: also passed as --session-id)
    pub workspace: PathBuf,             // cwd; must be a registered workspace
    pub resume: Option<VendorSessionRef>,
    pub permission_profile: PermissionProfile, // central policy, translated per vendor
    pub model: Option<String>,
    pub initial_input: Option<AgentInput>,
}

// Sketch only: async fns in traits are shown for readability; the real design
// will likely run each session as an actor task behind a command channel.
pub trait AgentAdapter: Send + Sync {
    fn descriptor(&self) -> &AgentDescriptor;
    async fn start(&self, spec: SessionSpec, sink: AdapterSink) -> Result<SessionHandle, AdapterError>;
}

pub trait SessionHandle: Send + Sync {
    async fn send_input(&self, input: AgentInput) -> Result<TurnRef, AdapterError>;   // new turn
    async fn steer(&self, turn: TurnRef, input: AgentInput) -> Result<(), AdapterError>; // if capable
    async fn interrupt(&self, turn: TurnRef) -> Result<(), AdapterError>;
    async fn resolve_approval(&self, id: ApprovalId, decision: ApprovalDecision) -> Result<(), AdapterError>;
    async fn stop(&self, mode: StopMode) -> Result<ExitInfo, AdapterError>; // Graceful (SIGINT → SIGTERM after grace) | Kill
    fn vendor_session(&self) -> Option<VendorSessionRef>; // Claude session_id / Codex threadId
    fn extension(&self, id: ExtensionId) -> Option<&dyn Any>; // typed vendor operations
}
```

`AdapterSink` is a bounded channel into Core's event engine. Adapters must not block on it
indefinitely. When the sink is full, they apply backpressure by pausing reads from the child's
stdout.

## Normalized events (proposed `event_type` values)

| Event | Durable | Payload highlights | Claude source | Codex source |
|-------|---------|--------------------|---------------|--------------|
| `session.started` | yes | vendor, cli_version, pid, vendor_session_id | `system/init` | `thread/started` |
| `session.state_changed` | yes | from, to | process + result | `thread/status/changed` |
| `turn.started` / `turn.completed` | yes | turn_ref, status (`completed\|interrupted\|failed`), usage, cost? | first message / `result` | `turn/started` / `turn/completed` |
| `item.started` / `item.completed` | yes | kind (`message\|reasoning\|command\|file_change\|tool_call\|plan`), summary, vendor_item | `assistant`/`user` tool_use + tool_result | `item/started` / `item/completed` |
| `item.delta` | **no** (ephemeral stream) | item ref, text chunk / output chunk | `stream_event` | `item/*/delta`, `outputDelta` |
| `approval.requested` | yes | kind (`command\|file_change\|permission\|tool`), command/cwd/diff, vendor request id | `--permission-prompt-tool` MCP call | `item/*/requestApproval` |
| `approval.resolved` | yes | decision, decided_by (`operator\|policy`), scope (`once\|session`) | MCP tool reply | response + `serverRequest/resolved` |
| `session.ended` | yes | exit code / signal, reason | process exit (143 = SIGTERM) | process exit |
| `adapter.warning` / `adapter.error` | yes | message, vendor detail | `api_retry`, stderr | `warning`, `error` |

Rules:

- Raw vendor payloads go in a `vendor` sub-object for diagnostics. Consumers rely only on the
  normalized fields.
- Durable events go through the journal (ADR 0004). `item.delta` uses a separate, bounded,
  lossy-by-design stream; the authoritative text is in `item.completed`.

## Approval decisions

```text
ApprovalDecision = Approve { scope: Once | Session } | Deny { reason } | Cancel
```

| Switchyard | Claude (`--permission-prompt-tool`) | Codex (`requestApproval`) |
|------------|-------------------------------------|---------------------------|
| Approve(Once) | allow | `accept` |
| Approve(Session) | allow, plus a session-scoped rule kept by Core | `acceptForSession` |
| Deny | deny, with reason | `decline` |
| Cancel | deny, plus interrupt the turn | `cancel` |

Vendor-only options are exposed as extensions, for example Codex `acceptWithExecpolicyAmendment`
and Codex `item/permissions/requestApproval` with granted subsets.

## Permission profiles

Core keeps central rules (the `permissions` table: scope project/agent, `allow|ask|deny`, a rule
string). An adapter translates the effective profile into vendor configuration at start:

- **Claude:** `--permission-mode`, `--allowedTools` / `--disallowedTools`, and a
  `--settings` JSON with `permissions.{allow,ask,deny}`. `ask` routes to Switchyard via the prompt
  tool.
- **Codex:** `sandbox`/`sandboxPolicy`, `approvalPolicy` (`on-request` routes to Switchyard; `never`
  only with read-only or pre-approved policies), and the writable roots of the workspace.

Unrepresentable rules must make `start()` fail with `AdapterError::UnsupportedPolicy`. They are never
silently weakened.

## Error model

`AdapterError` variants: `ExecutableNotFound`, `UnsupportedVersion { found, supported }`,
`HandshakeFailed`, `UnsupportedPolicy`, `CapabilityMissing(capability)`, `SessionNotRunning`,
`Vendor { code, message }`, `Io`. Every failure that ends a session also emits `session.ended` with a
reason.

## Conformance tests (to write with the first adapter)

- Use a **fake vendor binary** that replays recorded stream-json or JSON-RPC transcripts, so tests run
  without network access or credentials. This is a test fixture, not a UI simulator.
- Cover: start → turn → completion; interrupt mid-turn; approval request → each decision; crash →
  `session.ended`; resume using the vendor session ref; sink backpressure; unsupported version.
- Run optional live smoke tests behind an environment flag against the real CLIs.
