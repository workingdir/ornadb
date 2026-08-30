# Plans

This directory contains implementation and verification plans for changes that cross
storage, server, and client boundaries.

## Issue #440 — bounded SQLite runtime

| Plan | Scope | Status |
| --- | --- | --- |
| [004 — SQLite runtime/security parity](004-sqlite-runtime-security-parity.md) | The deliberately bounded local SQLite command and socket surface, including its security and persistence boundary | Implemented bounded slice; full PostgreSQL parity is not claimed |
| [005 — SQLite parity CI](005-sqlite-parity-ci.md) | Deterministic offline checks, process smoke coverage, and CI evidence | Implemented as a dedicated CI gate |

The SQLite implementation shares the neutral revision-store contract and canonical
source/artifact validation with PostgreSQL. It does **not** claim to implement the
PostgreSQL-only `invoke`, `state`, `inspect`, or `security` transports, and it has no
resource-dispatch or durable resource-audit path. The bounded differences are part of
the contract, not silent fallbacks.
