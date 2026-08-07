# ADR 0001: PostgreSQL Is a Private Kernel

**Status:** Accepted

## Decision

PostgreSQL is the sole initial OrnaDB backend. It is a private storage and
transaction kernel, not part of OrnaDB's public product contract.

Public clients use the Orna protocol. OrnaDB owns the language parser,
catalogue, semantic plan, execution plan, and returned values. It does not
offer public pgwire access or PostgreSQL compatibility for `psql`, PostgreSQL
drivers, ORMs, PostgreSQL DDL, or PostgreSQL-specific SQL.

An operator with server or host access can use `orna server backend-shell` for
backend administration. This escape hatch is not available through the public
protocol, Orna functions, or scripts.

## Precedence

This accepted amendment supersedes the conflicting or open parts of these
specification sources:

* `spec/docs/00-start-here.md` permits PostgreSQL or SQLite for the first
  proof.
* `spec/docs/36-storage-transactions.md` presents PostgreSQL and SQLite as
  prototype alternatives.
* `spec/docs/38-implementation-roadmap.md` permits PostgreSQL or SQLite.
* `spec/docs/12-object-relational-model.md` describes PostgreSQL wire
  compatibility as a possible direction.
* `spec/docs/41-open-questions.md` leaves PostgreSQL-backed storage and
  pgwire compatibility open.

For this subject, this record has precedence over those sources and their
derived examples.
