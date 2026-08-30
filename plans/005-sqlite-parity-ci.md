# Plan 005 — SQLite parity CI

## Purpose

Keep the local SQLite runtime buildable and executable without a PostgreSQL
service. The gate proves local persistence, source lifecycle, direct SERVER
invocation/raw-call, USER state, security administration, socket behavior, and
redacted inspection evidence without pretending to cover PostgreSQL CLIENT/Qt
execution or resource transport parity.

```text
just sqlite-check
just sqlite-smoke
```

`sqlite-check` runs offline Cargo checks for `orna-storage` and `orna-sqlite`, then
checks the `orna` binary. `sqlite-smoke` runs the deterministic
`revision_store_smoke` example and the focused `orna-server` `sqlite_backend`
integration target.

The CI job first runs `cargo fetch --locked` to make the subsequent offline
commands independent of registry availability; the recipes themselves never
need a PostgreSQL service.

## Evidence covered

The focused process target must retain coverage for:

- fresh-file source apply and a separate-process reopen/diff lifecycle;
- source diagnostics that leave the active pair and migration ledger unchanged;
- semantic diff that does not perform physical planning or mutation;
- read-only diff rejection for a fresh database path;
- direct SERVER invoke output formats, raw-call routing, USER-state write/load/
  conflict behavior, and security-admin persistence;
- persisted redacted invocation audit, inspection-summary, trace records, and
  reopen behavior;
- private socket mode, stale-socket handling, live-server conflict, graceful signal
  cleanup, unsupported-version rejection, and concurrent clients;
- V1 raw-call handshake/dispatch, unknown-target failure, and V2 catalogue calls.

The SQLite adapter tests additionally cover exact artifact persistence, recovery,
source/snapshot integrity, migration ordinal/format/version/canonical-byte/digest
corruption, unsupported capabilities, bounded evidence payloads, and concurrent
apply serialization.

## CI policy

The workflow has a dedicated `SQLite parity gate` job. It checks out the
PostgreSQL submodule recursively because the `orna-server` binary links the
embedded engine even when the exercised commands are SQLite LocalPath commands.
It runs both recipes on `ubuntu-latest` with Rust 1.95 and uploads command logs as
retained evidence. The existing Compose PostgreSQL gate remains separate and
continues to own PostgreSQL-only resource transport and protected-audit proofs.

A green SQLite gate is evidence of the local contract above. It is not evidence
of PostgreSQL CLIENT/Qt execution, resource durability, or value-bearing
inspection parity.
