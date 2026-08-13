# ADR 0032: Raw Calls Dispatch One-Column SERVER SELECTs

**Status:** Accepted

## Decision

The first raw SERVER dispatch extends the existing authenticated local raw-call
path. One complete call contains an active `FunctionId` and no arguments. The
kernel, not `orna-server`, selects the active function domain and executes
exactly one of the already accepted protected operations:

* a parameter-free CLIENT function through the current Boolean evaluator; or
* a parameter-free SERVER `SELECT` whose declared result has exactly one
  column and whose returned values belong to the protocol-1 runtime subset.

The protocol-1 runtime subset is Boolean, integer, big integer, float, text,
bytes, typed scalar null, or typed object reference. Enum and record results
remain closed in this first SERVER dispatch even on protocol 2 or 3. This
keeps one dispatch result valid under every currently negotiated connection
without making protocol-version selection a kernel input. Multi-column rows,
arguments, mutations, and streams remain deferred.

The public kernel interface is:

```text
dispatch_authenticated_raw_call(
    session: &AuthenticatedSession,
    function: FunctionId,
) -> Result<AuthenticatedRawCallResult, PostgresKernelError>
```

`AuthenticatedRawCallResult` is a closed enum. Its CLIENT variant owns one
`RuntimeValue`. Its SERVER variant owns zero or more `RuntimeValue` values in
query row order. The SERVER variant contains no column name, row wrapper,
PostgreSQL value, or encoded frame. A consuming result interface transfers the
validated values without cloning their payloads.

The raw adapter calls only this interface for a parameter-free call. It does
not call the CLIENT operation and then probe the SERVER operation. It does not
recover the catalogue, inspect a function domain, make a security decision,
or choose a revision itself. The existing direct CLIENT and SERVER operations
remain focused kernel and test surfaces, but they are not alternative raw
dispatch paths.

## One decision and transaction

The kernel opens one read-write, `REPEATABLE READ` transaction and performs
these steps in order:

1. require the current migrations;
2. recover and verify one complete active revision;
3. recover the security snapshot against that same active revision;
4. construct the exact `InvocationTarget` from the requested `FunctionId` and
   recovered active `RevisionPair`;
5. revalidate and authorise the trusted authenticated session;
6. append one protected `EXECUTE` audit decision; and
7. on an allowed decision, select the active function domain and execute only
   its accepted CLIENT or SERVER path under that same revision and decision.

An unknown function is denied by the security snapshot before domain
selection. It produces the same committed denied audit and typed denial as the
existing protected operations. The adapter cannot use domain selection as a
function-existence oracle.

An allowed CLIENT decision evaluates through the existing authorised CLIENT
entry. The audit append and evaluation retain the current CLIENT transaction,
commit, and shutdown semantics.

An allowed SERVER decision appends its audit before target-shape validation.
It then creates one savepoint, requires the exact parameter-free,
one-result-column, supported-value boundary, and executes through the existing
`AuthorisedInvocation` SERVER SELECT entry. A successful target releases the
savepoint and commits the outer transaction. A target validation or execution
failure rolls back the savepoint, commits the allowed audit, and returns the
typed target failure. Savepoint, audit, commit, driver, and shutdown failures
remain operational failures. The kernel does not retry or make a second
decision.

The operation recovers the active revision and security snapshot once. Domain
selection, active revision, authorisation, audit evidence, function revision,
plan, execution, and result validation therefore cannot come from different
snapshots.

## Result adaptation

The raw adapter emits these actions after the existing `CALL_ACCEPTED`:

```text
CLIENT value               EVENT_BATCH(value), CALL_COMPLETED
SERVER zero rows            CALL_COMPLETED
SERVER row value            one EVENT_BATCH(value) per row, then CALL_COMPLETED
```

Every SERVER row has exactly one value. Values retain query row order. Each
row is a separate `ServerAction::Events` action on `RESULT_VALUES`; the
existing protocol state machine assigns the contiguous event sequence,
charges exact window credit, and stops at the first action without sufficient
credit. The adapter does not concatenate rows, fabricate a table shape, emit
column metadata, or encode a frame itself. Existing SERVER row, cell, and
16-MiB logical payload bounds limit the complete retained result.

The adapter keeps the complete result and its shared kernel-operation permit
until all actions are delivered or discarded. A disconnected peer or server
shutdown still drains the accepted protected operation. Cancellation never
drops the kernel future. After completion, cancellation may replace a clean
CLIENT or SERVER result and an expected denial, but it cannot replace an
operational failure.

Completed outbound result values are bounded by the SERVER executor's one
16-MiB logical result limit and remain owned by their dispatch completion.
They do not consume the listener's separate 256-MiB budget for declared
inbound client-frame payload bytes. The socket moves `DispatchGuards` into the
pending completion when the kernel future finishes and releases them only
when that completion is fully delivered, cancelled, or discarded during
connection drain.

## Closed failures

The public mapping is:

```text
kernel outcome                              public call outcome
CLIENT or SERVER value result               value events and completion
ClientExecuteDenied or ServerExecuteDenied  EXECUTE_DENIED
ClientExecution                             CLIENT_EVALUATION_FAILED
unsupported raw SERVER target shape         TARGET_UNAVAILABLE
pure SERVER target validation failure       TARGET_UNAVAILABLE
every operational kernel failure            INTERNAL_FAILURE
```

The kernel classifies a SERVER target failure before it crosses the raw
adapter boundary. PostgreSQL, recovery, invariant, audit, savepoint, commit,
driver, shutdown, row-decode, or canonical-value-decode failures are
operational. They cannot be converted to `TARGET_UNAVAILABLE` or hidden by
cancellation. A closed signature, unsupported domain value, unsupported
artifact or plan, row/cell/payload limit, and other source-independent target
validation failure is a pure unavailable-target outcome.

Every private typed source remains available to trusted diagnostics and never
enters a protocol frame. Public failures contain no function-existence fact,
domain, signature, revision, plan, SQL, PostgreSQL text, principal, role,
argument, row, or value.

## Required proof

Tests must prove:

* one parameter-free raw CLIENT call retains its exact current result, audit,
  failure, cancellation, and byte behaviour;
* one parameter-free one-column SERVER SELECT reaches only the unified kernel
  entry, commits one allowed audit, and emits exact values in query row order
  followed by completion;
* a zero-row SERVER result emits completion without an empty event batch;
* two or more declared columns, any parameter, a mutation, a non-SERVER
  function at the SERVER executor, enum or record output, and every unknown
  future runtime value fail closed before a public value event;
* an unknown function and missing, stale, or invalid session produce the exact
  redacted denied outcome and one committed denied audit without domain
  probing or private data SQL;
* domain selection, decision, audit, revision, target execution, and result
  all use one repeatable-read snapshot during a concurrent active revision or
  security change;
* target validation and execution failures retain the allowed audit and the
  exact pure-target or operational classification;
* audit, savepoint, rollback, release, commit, driver, and shutdown failures
  return `INTERNAL_FAILURE`, retain their private source, fabricate no success,
  and cannot be hidden by cancellation;
* every SERVER row becomes one ordered `RESULT_VALUES` event action, window
  exhaustion retains the first unsent action unchanged, and later credit
  resumes without duplicate or skipped sequence numbers;
* peer close, listener shutdown, and cancellation drain the accepted kernel
  work and release all result, payload, connection, and kernel-operation
  resources; and
* protocol-1, protocol-2, and protocol-3 connections return scalar/reference
  values with identical tag, `TypeId`, payload length, and payload bytes after
  the four-byte version marker, while each connection selects its exact
  `ORV1`, `ORV2`, or `ORV3` marker and preserves marker closure.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
live PostgreSQL, raw-socket, and security-audit gates remain required.

## Implementation sequence

1. Accept and index this raw SERVER dispatch boundary.
2. Add a consuming, payload-preserving result-row interface in `orna-core`.
3. Add the unified kernel result, one-transaction domain routing, exact raw
   target-shape validation, and target-failure classification in at most three
   kernel files.
4. Route parameter-free calls through the unified entry in
   `raw_client_dispatch.rs`, preserve the record-argument preflight and every
   non-empty closed outcome, and move completed-dispatch guard ownership into
   `raw_socket.rs` pending state until delivery or discard.
5. Add focused unit and live PostgreSQL/socket proof in the same two server
   modules and `standard_database.rs` for CLIENT preservation, SERVER rows,
   zero rows, denials, unavailable targets, operational failures, snapshot
   races, flow control, cancellation, and cleanup.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

## Deferred surface

This decision does not define function-name resolution, CLI argument parsing,
default expressions, a row or table event, multi-column results, enum or
record SERVER results, mutation dispatch, general argument binding, TCP/TLS,
`sys.invoke`, presenters, `--output`, `--explain`, CLIENT artifacts, runtime
selection, deadlines, policies, or public audit inspection.

It is a useful raw recovery and diagnostic execution slice. It is not the
universal invocation system and does not add an `orna invoke` command.

## Precedence

For parameter-free raw calls, this decision extends ADR 0027 with kernel-owned
CLIENT/SERVER domain routing and extends ADR 0030 with the first raw adapter.
It preserves ADRs 0026, 0028, and 0031 for state, transport, resource, codec,
and record-argument preflight behaviour. The one-transaction rule has
precedence over any adapter-side domain probe.

This decision follows the milestone-4 raw invocation boundary in
`spec/docs/38-implementation-roadmap.md` and deliberately stops before the
milestone-5 `sys.invoke` contract in `spec/docs/13-invocation-system.md`.
