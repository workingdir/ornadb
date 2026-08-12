# ADR 0027: Raw CLIENT Dispatch Preserves the Protected Kernel Gate

**Status:** Accepted

## Decision

The first raw-call dispatcher adapts a complete ADR 0026 `RawCall` to the
existing protected CLIENT execution operation. It lives in `orna-server`,
after authenticated transport state and before the PostgreSQL kernel. It does
not recover security state, authorise, audit, or evaluate a function itself.

The dispatcher receives these trusted inputs separately:

* a cloned `PostgresKernel` connection point;
* the `AuthenticatedSession` established by the transport adapter;
* the connection-local stream number; and
* the complete `RawCall`, which contains only a function and ordinary typed
  arguments.

It creates one fresh `InvocationId` and exposes one `CALL_ACCEPTED` server
action before asynchronous execution starts. The invocation identity never
comes from the client and is not durable decision evidence.

The current CLIENT evaluator accepts only parameter-free Boolean constants.
A raw call with any argument is accepted as a protocol call, then finishes as
`TARGET_UNAVAILABLE` without opening PostgreSQL. This outcome depends only on
the supplied call shape and does not reveal whether the function exists.

For a call with no arguments, the dispatcher invokes only
`PostgresKernel::evaluate_client_function`. That kernel operation revalidates
the authenticated session, pins the active revision, makes and audits the
`EXECUTE` decision, and evaluates only an allowed CLIENT function.

## Exact outcome mapping

One finished dispatch has one of these closed results:

```text
kernel result                              protocol actions after acceptance
CLIENT value                              EVENT_BATCH(value), CALL_COMPLETED
ClientExecuteDenied                       CALL_FAILED(EXECUTE_DENIED)
ClientExecution                           CALL_FAILED(CLIENT_EVALUATION_FAILED)
every other PostgresKernelError           CALL_FAILED(INTERNAL_FAILURE)
non-empty raw argument list               CALL_FAILED(TARGET_UNAVAILABLE)
```

The value event uses `RESULT_VALUES`. The ADR 0026 connection state machine
assigns its sequence and enforces its byte window. The dispatcher does not
encode frames, consume credit, or buffer another copy of the result.

Every kernel failure remains available as a typed private error source in the
dispatch result. It can contain internal error text and must not cross the
protocol seam. Protocol actions contain only the closed redacted failure. They
never contain a PostgreSQL error, denial reason, source text, principal, role,
revision, function-existence fact, or arbitrary message.

## Cancellation boundary

The transport adapter applies `CALL_CANCEL` through the ADR 0026 state machine.
For the current bounded Boolean evaluator, it does not abort a protected kernel
future. Acceptance commits the adapter to start and complete `finish`, even if
cancellation arrives before `finish` is first polled. The adapter waits for all
required kernel decision, audit, transaction, and session-shutdown work.

After `finish`, an operational kernel failure takes precedence over pending
cancellation and produces `CALL_FAILED(INTERNAL_FAILURE)`. Otherwise the
adapter discards the finished dispatch actions and applies one
`CALL_CANCELLED`. Expected security denial and pure evaluator failure are call
outcomes, so cancellation can replace them. This prevents cancellation from
skipping required work or hiding an audit, commit, driver, or shutdown failure.

A later executor that can block or stream must add a kernel-owned cancellation
operation before the adapter may interrupt it. Dropping a kernel future is not
an accepted cancellation mechanism.

## Interface

`RawClientDispatch::new` accepts the trusted inputs and cannot fail. It exposes
the initial accepted `ServerAction`, then consumes itself in one asynchronous
`finish` operation. `RawClientDispatchResult` exposes its ordered terminal
actions and an optional borrowed `PostgresKernelError` source.

The interface does not accept a principal, role list, revision pair,
authorisation decision, evaluator, frame bytes, window value, or cancellation
flag. Tests and a later Unix-socket adapter use the same public interface.

## Required proof

Tests must prove:

* every dispatch creates a fresh invocation identity and the exact accepted
  action for its stream;
* a non-empty argument list performs no PostgreSQL operation and produces only
  the accepted action followed by redacted `TARGET_UNAVAILABLE`;
* an authorised Boolean CLIENT call produces one typed value event then
  completion, bound to the same stream;
* missing grant, stale session, and unknown function all produce the same
  public `EXECUTE_DENIED` action while retaining their distinct private typed
  sources;
* a pure evaluator failure produces only `CLIENT_EVALUATION_FAILED` after
  acceptance;
* database, migration, audit, commit, driver, and shutdown failures produce
  only `INTERNAL_FAILURE` after acceptance;
* protocol actions contain no authentication input or unredacted text, while a
  private dispatch result retains its typed kernel source;
* success, expected denial, and pure evaluator failure commit their required
  audit evidence, while pre-decision and audit or commit failures fabricate no
  evidence; and
* cancellation before the first `finish` poll still starts and completes all
  required kernel work, closes the session, discards clean call outcomes, and
  cannot mask an operational kernel failure.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, and focused
live PostgreSQL gates remain required.

## Implementation sequence

1. Accept this exact raw CLIENT dispatch and error-redaction boundary.
2. Add the server dispatcher and pure mapping tests.
3. Add focused live tests for success, security denials, evaluator failure,
   audit failure, and session closure.
4. Add the authenticated Unix-socket adapter that drives ADR 0026 state and
   this dispatcher.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This decision does not define SERVER dispatch, argument binding, defaults,
`sys.invoke`, name resolution, active-role selection, presenter planning,
artefact transfer, socket framing, deadlines, or interruptible kernel work.

## Precedence

This composes work ADRs 0022, 0024, and 0026 for the current parameter-free
Boolean CLIENT subset. It advances the protected raw-call dispatcher required
by milestone 4. It does not mark the local socket or universal invocation
checklist rows complete.
