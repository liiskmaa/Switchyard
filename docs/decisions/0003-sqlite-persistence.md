# ADR 0003: SQLite (WAL) with embedded, forward-only migrations

- Status: accepted (2026-11-11)

## Decision

- `rusqlite` with the `bundled` feature, so a known SQLite version is used regardless of the system.
- `journal_mode=WAL`, `synchronous=FULL` (every commit is fsynced, because the journal must survive
  power loss and write volume is low), `foreign_keys=ON`, and a 5 s busy timeout.
- One write connection behind a `std::sync::Mutex`. Async callers use
  `Storage::call` → `spawn_blocking`.
- Migrations are SQL files in `/migrations`, embedded with `include_str!` and tracked with
  `PRAGMA user_version`. Each migration runs in a transaction together with the version bump. Core
  refuses databases with a newer version.
- `STRICT` tables, UUIDv7 text keys, and Unix-millisecond integer timestamps. JSON columns are
  validated with `json_valid`.

## Rejected alternatives

- `sqlx`: async SQLite still runs on a thread internally. It adds macros and compile-time
  `DATABASE_URL` handling with no benefit for an embedded database.
- `refinery` or `rusqlite_migration`: about 40 lines of code cover what is needed.
- Connection pool: unnecessary with one writer. It can be added for read-heavy queries later without
  changing the API.
