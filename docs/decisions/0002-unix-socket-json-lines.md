# ADR 0002: Unix domain socket with newline-delimited JSON

- Status: accepted (2026-11-11)

## Options considered

| Option | Pros | Cons |
|--------|------|------|
| **Unix domain socket + JSON lines** | Filesystem permissions give access control; no port allocation or TCP exposure; `getpeereid` gives the peer UID; trivial to debug (`nc -U`, Python); supported natively by Tokio | 104-byte path limit on macOS; not available on Windows (named pipes would be needed there) |
| Localhost TCP / WebSocket | Universal tooling | Reachable by any local process and by browsers (DNS rebinding, CSRF-style attacks); needs token authentication and port management |
| gRPC over UDS | Schemas, codegen | Heavy dependencies (tonic, prost, protoc); harder to inspect; little gain for one client |
| XPC / Mach ports | Native to macOS | Not portable; awkward from Rust; blocks Linux CI |
| stdio (Core as a child of the UI) | Simple | Contradicts ADR 0001: Core would die with the UI |

## Decision

A Unix domain stream socket at `<data dir>/core.sock` (mode `0600`, data dir `0700`), with an
additional check that the peer UID equals the socket owner. Frames are one JSON object per line, with
a 1 MiB limit. Messages use a request/response/event envelope with a `type` tag and a version
negotiated by `initialize`. Full specification: [core-ipc.md](../protocols/core-ipc.md).

## Consequences

- Security relies on OS user separation. Any process running as the same user can use Core, which is
  the same trust boundary that the agents themselves operate in.
- JSON costs CPU compared with a binary encoding. At expected volumes (agent output deltas: hundreds
  of events per second at most) this is negligible.
- The design is close to the JSON-RPC-over-JSONL that Codex App Server uses, which keeps adapter code
  familiar. It is not literally JSON-RPC 2.0, because it uses a `type` tag and pushes events by
  subscription.
- Windows support would need a named-pipe transport behind the same codec.
