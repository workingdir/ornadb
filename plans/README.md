# Plans

This directory contains implementation and verification plans for changes that cross
storage, server, and client boundaries.

## Issue #440 — bounded SQLite runtime

| Plan | Scope | Status |
| --- | --- | --- |
| [004 — SQLite runtime/security parity](004-sqlite-runtime-security-parity.md) | The local SQLite command, socket, security, USER-state, invocation, and redacted inspection surface | Implemented local parity slice; full PostgreSQL parity is not claimed |
| [005 — SQLite parity CI](005-sqlite-parity-ci.md) | Deterministic offline checks, process smoke coverage, and CI evidence | Implemented as a dedicated CI gate |

The SQLite implementation shares the neutral revision-store contract and canonical
source/artifact validation with PostgreSQL. `LocalPath` now routes source apply/diff,
server-only invoke and raw-call, USER state, security administration, and redacted
invocation inspection directly to SQLite. It does **not** claim PostgreSQL parity for
CLIENT/Qt execution, resource transport or resource-audit payloads, standard-backed
protected transports, or value-bearing inspection data. Unsupported commands fail
closed; they do not silently fall back to PostgreSQL.
