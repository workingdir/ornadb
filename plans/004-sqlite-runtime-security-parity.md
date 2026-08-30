# Plan 004 — SQLite runtime/security parity

## Decision

Issue #440 delivers a bounded local SQLite runtime. The adapter uses the same
backend-neutral `ApplicationRevisionStore` contract as PostgreSQL, but parity means
shared invariants and explicit failure boundaries—not identical command coverage.

## Implemented contract

- `--db <path> source apply <file>` opens or creates the caller-owned SQLite file,
  checks and prepares the source, validates the exact compiler physical artifact,
  and atomically persists the active revision, source/catalogue lineage, snapshot,
  and migration ledger.
- `--db <path> source diff <file>` requires an existing regular database file,
  opens it read-only, performs no bootstrap or physical migration planning, and
  renders the semantic diff without changing the active pointer or ledger.
- `--db <path> server run` owns a private owner-only Unix socket at
  `<database>.orna.sock`. The socket accepts the bounded raw-call protocol, refreshes
  the durable active revision for each new connection, enforces connection and frame
  limits, and removes its socket on graceful shutdown.
- The SQLite physical surface is closed: unsupported value, enum, record, binding,
  scalar, and artifact shapes fail before durable mutation. The server-plan and
  parameter-echo result shapes are the only execution shapes in this slice.
- Recovery recomputes source-unit, source-bundle, source-revision, catalogue, and
  physical-ledger integrity, and rejects corrupted or mismatched snapshots and
  lineage before exposing state.

## Explicit non-goals

SQLite LocalPath does not route PostgreSQL-only `invoke`, `state`, `inspect`, or
`security` commands. It also does not implement the authenticated resource transport,
resource invocation, durable resource audit rows, or PostgreSQL protected audit
semantics. These are rejected or absent by design; no local fallback is implied.

The socket acknowledges protocol versions one through five. Versions one through
three have typed SQLite handling in this bounded slice; versions four and five use
the protocol fallback when the required opaque codec registry is unavailable. A
successful handshake is not a claim of full feature parity.

## Verification surface

- `just sqlite-check` compiles the storage and SQLite packages and the `orna` binary
  offline.
- `just sqlite-smoke` runs the deterministic revision-store example and the focused
  SQLite process/socket integration target.
- `just kernel-resource-audit-proof` remains the PostgreSQL-only proof for resource
  values, window enforcement, redacted audit recovery, and reopen behavior.
