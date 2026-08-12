# ADR 0024: Security Decisions Append Protected Audit Records

**Status:** Accepted

## Decision

The milestone-3 audit skeleton records authentication and function `EXECUTE`
decisions in one protected, append-only kernel relation. It is decision
evidence, not a general logging framework.

Each record has:

* a stable opaque `SecurityAuditEventId`;
* a database-generated sequence and recording time;
* one closed event kind: `AUTHENTICATION` or `EXECUTE`;
* one closed outcome: `ALLOWED` or `DENIED`;
* the session principal when one is known;
* for `EXECUTE`, the exact `FunctionId` and pinned `RevisionPair`;
* the effective and authorising principals only for an allowed decision; and
* one closed denial reason when the outcome is denied.

The denial reasons are the existing typed authentication and execute denial
families. Audit records never contain a Linux UID, credential material,
principal supplied by a request, role supplied by a request, source text,
arguments, results, arbitrary error text, or an extensible payload.

The relation grants no authority to `PUBLIC`. Clients cannot construct or
append audit records. A protected kernel inspection operation may recover
records in database sequence order; a public administrative view and
redaction policy remain deferred.

## Transaction boundary

Local authentication becomes a read-write repeatable-read transaction. After
recovering the active security snapshot, it appends exactly one allowed or
expected-denied authentication record and commits before returning the session
or typed denial. An audit insert, commit, or database-session shutdown failure
fails the authentication operation. A failure before a valid security decision
exists is not converted into a fabricated audit event.

CLIENT execution appends the `EXECUTE` decision made against the same active
snapshot and revision pair. A denied decision is committed before its typed
denial returns. An allowed decision is appended after authorisation and before
commit; a pure evaluator failure does not erase the already-made decision.
No successful or denied decision may return if its audit transaction fails.

Audit insertion does not grant execution and does not replace
`AuthorisedInvocation`. The allowed value from ADR 0020 remains the sole input
to CLIENT evaluation and later SERVER execution.

## Storage shape

Migration 11 adds `_orna_kernel.security_audit_events`. PostgreSQL checks exact
sixteen-byte identities, closed kind/outcome/reason spellings, paired revision
columns, and the permitted nullability shape for each event. The generated
sequence establishes durable order; it is not an Orna identity. The recording
time is operational evidence and is not used to make an authorisation
decision.

The kernel generates the event identity. Replacement of the security snapshot
does not delete audit history. Recovery rejects malformed durable rows rather
than repairing, skipping, or reinterpreting them.

## Required proof

Tests must prove:

* successful local authentication appends one allowed record without a UID;
* unknown and invalid mapped principals append the exact denied reason;
* direct and selected-role `EXECUTE` grants append their exact authorising
  evidence;
* missing grant, invalid session, and unknown function each append one exact
  denied record;
* revoking credentials or grants does not delete earlier records;
* reconnect recovery preserves event identities, order, principals, targets,
  revisions, outcomes, and reasons;
* malformed rows, incomplete pairs, and invalid nullability shapes fail closed;
  and
* every path closes its PostgreSQL session.

Normal format, strict Clippy, rustdoc, diff, similarity, migration, and live
PostgreSQL gates remain required.

## Implementation sequence

1. Define the audit identity and closed core record model.
2. Add and verify migration 11.
3. Recover protected audit history through one kernel interface.
4. Append local-authentication decisions atomically.
5. Append CLIENT `EXECUTE` decisions atomically.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This record does not define durable session rows, login secrets, enrolment,
external providers, delegation, role selection, object policies, capability
decisions, invocation lifecycle events, request tracing, retention, export,
redaction views, `sys.invoke`, or a public audit protocol.

## Precedence

This implements the audit skeleton required by milestone 3 and the security
decision evidence described by the canonical invocation flow. It narrows the
general audit catalogue in `spec/docs/35-security.md` to the first decisions
that the current trusted kernel can actually make.
