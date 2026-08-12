# ADR 0030: One Authenticated, Authorised, Audited SERVER SELECT

**Status:** Accepted

## Decision

The first authenticated SERVER operation is one bounded `SELECT` operation
with this trusted kernel interface:

```text
execute_authenticated_server_select(
    session: &AuthenticatedSession,
    function: FunctionId,
    arguments: &[FunctionArgument],
) -> Result<ServerSelectResult, PostgresKernelError>
```

`AuthenticatedSession` is trusted authentication state established by the
existing session boundary. `FunctionId` is the stable target identity. The
argument slice is the existing typed `FunctionArgument` list. The operation
does not decode credentials, a principal, active roles, a function name, or a
new argument format. It does not accept an `AuthorisedInvocation` from the
caller. The kernel creates that value only after it has made the decision.

The operation performs one PostgreSQL operation in one read-write,
`REPEATABLE READ` transaction:

1. require the current migrations;
2. recover the active database revision and its complete active catalogue;
3. recover the durable security snapshot against that exact active revision;
4. construct the exact target from the requested `FunctionId` and the active
   `RevisionPair`;
5. revalidate the trusted session against the recovered snapshot;
6. authorise that exact target;
7. append one protected `EXECUTE` audit decision; and
8. execute the target only through the resulting `AuthorisedInvocation`,
   using the same recovered active revision and transaction snapshot.

The operation recovers the active revision and security snapshot once. It
does not re-read either source after authorisation, and it does not call the
existing unauthenticated SERVER entry point as a second operation. The
authorised execution helper must verify that its invocation target and active
revision are the same pair and function that the decision covered. It then
uses the existing SERVER SELECT validation, plan, argument, result-shape,
limit, and stable-identity rules. It does not add a second SERVER SELECT
language form.

The existing unauthenticated execution method remains a private development
and compatibility surface until a later decision removes or replaces it. It
is not the authenticated boundary and must not be used by an external
dispatch path.

## Transaction and savepoint semantics

The outer transaction is read-write because the protected audit record is a
durable result of the security decision. The security decision and its audit
record are made before target validation or data execution.

An expected denial has this exact outcome:

```text
BEGIN REPEATABLE READ
  recover active + security
  revalidate session
  deny exact target
  append one DENIED EXECUTE audit event
COMMIT
return ServerExecuteDenied
```

The denial audit must commit before the typed denial returns. No target
validation, private data statement, or SERVER executor is entered. An audit
insert failure, outer commit failure, or PostgreSQL session shutdown failure
returns an operational kernel error instead of the expected denial.

An allowed decision follows this shape:

```text
BEGIN REPEATABLE READ
  recover active + security
  revalidate session
  allow exact target and create AuthorisedInvocation
  append one ALLOWED EXECUTE audit event
  SAVEPOINT server_select_execution
    validate and execute through AuthorisedInvocation
  RELEASE SAVEPOINT server_select_execution
COMMIT
return ServerSelectResult
```

The savepoint starts after the audit insert. If the target produces a pure
SERVER SELECT validation or execution error, the kernel rolls back to the
savepoint, keeps the audit insert, commits the outer transaction, and returns
the original typed target error. This includes a rejected active target,
artifact or plan validation, argument validation, result validation, and a
target execution failure. The operation never changes such an error into an
empty result or a successful `ServerSelectResult`.

Savepoint rollback, savepoint release, outer commit, or session shutdown is
part of the kernel operation. If any of those operations fails, the kernel
returns the operational failure. It does not claim that the target succeeded
because an allowed audit decision was recorded. If the audit insert or outer
commit fails, no target result or expected denial is returned. If PostgreSQL
has made the transaction unusable and the audit cannot commit, the operation
fails closed; it does not attempt a second decision or an automatic retry.

On successful target execution, the savepoint is released and the outer
transaction commits before the result is returned. A result is successful
only when target execution, audit durability, outer commit, and session
shutdown all succeed. A shutdown failure after PostgreSQL confirms commit
therefore remains an operational error, as in the existing SERVER execution
contract.

The audit event uses the existing closed ADR 0024 shape. An allowed event is
constructed from the exact `AuthorisedInvocation`. A denied event is
constructed from the trusted session, exact target, and existing typed
`ExecuteDenial`. The audit event contains no arguments, source text, result,
credential, request principal, or arbitrary error text.

## Security and execution boundary

The request cannot select an identity. The recovered snapshot is the sole
authority for session validity, active-role selection, known function
membership, active revision, and `EXECUTE` grants. A stale, disabled,
unknown, or otherwise invalid trusted session is an expected denied decision
and is audited as such.

The executor receives only `AuthorisedInvocation` evidence. It cannot receive
an arbitrary `FunctionId` through this operation, substitute another
`RevisionPair`, or use a separate principal or role list. Argument validation
still uses the supplied stable `ParameterId` values and active function
signature. It does not perform name lookup, default evaluation, implicit
conversion, or a new wire decode.

This operation is read-only with respect to application objects and active
catalogue state. The audit append is its only durable mutation. It supports
the current immutable SERVER SELECT forms, including the accepted
parameter-free, identity-selected, Boolean-predicate, and `SELECT DISTINCT`
forms, subject to their existing signatures and limits.

## Required proof

Tests must prove:

* an allowed direct grant executes the exact SERVER function and returns rows
  from the active `RevisionPair` and function revision recorded by the result;
* a selected-role grant produces the exact authorising principal in the
  allowed audit event and cannot be replaced by an unselected role;
* an invalid, disabled, stale, or unknown session, an unknown function, a
  stale target, and a missing grant produce one exact denied audit event,
  commit it, return `ServerExecuteDenied`, and issue no private data SQL;
* the allowed audit event is appended before target validation and execution,
  and contains the exact session, effective, authorising, function, and
  revision evidence from `AuthorisedInvocation`;
* duplicate, unknown, missing, null, wrong-type, and wrong-target arguments
  after an allowed decision retain the allowed audit event, commit it through
  the savepoint path, and return the typed target error rather than success;
* malformed plans, stale or mismatched artefacts, result-shape failures, row
  limits, and target PostgreSQL failures retain the allowed audit event and
  return the target or operational error without returning a successful
  result;
* a change to active security or the active revision cannot produce a mixed
  decision and execution, because both use one repeatable-read snapshot;
* the executor receives only `AuthorisedInvocation` for this operation and
  cannot be called with a bare function identity at the authenticated seam;
* audit insert, savepoint, rollback, release, commit, and session shutdown
  failures fail closed and never become a successful result or an unrecorded
  expected denial; and
* every success, denial, target failure, cancellation-free error, and cleanup
  path closes its PostgreSQL session without an automatic retry.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
live PostgreSQL, and authenticated SERVER execution gates remain required.

## Implementation sequence

1. Add the authenticated SERVER operation and its typed error/audit outcome
   seam in `crates/orna-kernel-postgres/src/security.rs` and
   `crates/orna-kernel-postgres/src/server_execution.rs`.
2. Make the server execution helper accept and verify
   `AuthorisedInvocation` while preserving the existing unauthenticated
   SERVER SELECT behaviour and result contract.
3. Export the new error surface from
   `crates/orna-kernel-postgres/src/lib.rs`, then add focused live PostgreSQL
   proof for allowed, denied, savepoint, failure, audit, and cleanup paths.

Each implementation commit changes one to three files and keeps the
repository buildable. The implementation must not modify the raw socket,
protocol frames, client dispatch, or canonical value codec as part of this
decision.

## Deferred surface

This record does not define or implement mutations, raw sockets, protocol
frames, row wire codecs, function names, default expressions, general
argument binding, `sys.invoke`, presenters, records, opaque values, streaming,
cancellation, deadlines, `SECURITY DEFINER`, object policies, capability
offers, or public audit inspection.

It does not make the authenticated SERVER operation a general invocation
system. Those surfaces require separate accepted decisions and must preserve
the same trusted-session, exact-target, pinned-revision, audit, and
fail-closed rules where they apply.

## Precedence

This decision composes work ADRs 0020, 0021, 0022, and 0024. It closes the
authenticated SERVER execution gap left by the current `server_execution` and
security APIs. It follows the invocation, security, and milestone boundaries
in `spec/docs/13-invocation-system.md`, `spec/docs/35-security.md`, and
`spec/docs/38-implementation-roadmap.md`.

For this one authenticated, authorised, audited SERVER SELECT operation, this
accepted record has precedence over the existing unauthenticated execution
entry point. It does not change the canonical language contract or the
accepted semantics of the existing SERVER SELECT forms.
