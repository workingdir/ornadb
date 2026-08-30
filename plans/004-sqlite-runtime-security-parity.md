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
- Recovery recomputes source-unit, source-bundle, source-revision, catalogue, and
  physical-ledger integrity, and rejects corrupted or mismatched snapshots and
  lineage before exposing state.
- `--db <path> invoke` resolves and executes the supported SERVER-function subset
  directly, performs canonical CLI binding, authenticates the local peer, and
  renders canonical, JSON, table, or CSV output. `--explain` is a redacted plan
  record and does not execute.
- `--db <path> raw-call` directly executes a supported SERVER function using
  bounded canonical ORV5 values from standard input. The private SQLite socket
  remains available for protocol clients and applies the same execute gate.
- `--db <path> state get|set` persists principal-scoped USER-state cells with
  canonical ORV5 values, optimistic revisions, type checks, and conflict results.
- `--db <path> security ...` persists principals, roles, memberships, execute
  grants, privilege grants, and local peer credentials. Mutations require the
  durable `SecurityAdmin` privilege and reads use the same snapshot model.
- Successful local invocations persist only redacted audit, inspection-summary,
  and trace evidence. Arguments, result values, source text, and resource
  payloads are never stored by this evidence path.

## Explicit non-goals

SQLite LocalPath does not implement CLIENT or Qt execution, the authenticated
resource transport, resource invocation, durable resource-audit payloads, or the
PostgreSQL standard-library protected transports. Inspection is intentionally
redacted and bounded: it exposes structural invocation summaries and trace
events captured by the local route, not value-bearing projections or source text.
The local raw-call and invoke routes accept SERVER functions only.

These are rejected or absent by design; no local fallback is implied.

## Verification surface

- `just sqlite-check` compiles the storage and SQLite packages and the `orna` binary
  offline.
- `just sqlite-smoke` runs the deterministic revision-store example and the focused
  SQLite process/socket integration target.
- `just kernel-resource-audit-proof` remains the PostgreSQL-only proof for resource
  values, window enforcement, redacted audit recovery, and reopen behavior.
