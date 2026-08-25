# ADR 0070: CLIENT USER State Lifecycle

**Status:** Accepted

## Decision

The CLIENT runtime connects declared `STATE ... SCOPE USER` slots to the
protected `sys.state.*` service through an explicit state-session boundary.
The authenticated adapter/session boundary establishes an opaque
authenticated-session binding on the caller-owned store before USER-state load
or flush. The client owns state values
and scheduling. The server owns principal selection, type validation against
the active catalogue, durable storage, and revision conflict decisions.

A client state session has one root invocation identity:

```text
root function
state profile
```

Every mounted function instance adds:

```text
function id
function instance key
state slot id
```

The complete client key is therefore:

```text
root_function_id
root_state_profile
function_id
function_instance_key
state_slot_id
```

The empty profile and empty instance key retain their existing meanings: the
default root profile and the default function instance. A key component that
contains a NUL byte is rejected before it reaches a transport or a local
state map.

## Client state store

`ClientStateStore` remains caller-owned. Its `LOCAL` and `SESSION` maps retain
in-memory values. Its `USER` map stores the loaded typed value, its persisted
`TypeId`, and its server revision. USER entries also track whether the local
value changed after load. The store also retains one opaque
authenticated-session binding for its USER cache. The authenticated adapter
establishes that binding before a USER load or flush. A load or flush using a
store already bound to a different session is rejected before transport access
and leaves the caller-owned values unchanged. This is intentional fail-closed
session-affinity behaviour, not a principal-selection mechanism.

The store exposes four lifecycle operations:

1. load a batch of authenticated `UserStateCell` values;
2. update a USER value through an explicit local operation;
3. build the currently dirty values as `UserStateChange` records; and
4. apply the aligned `UserStateWriteResult` records after a server flush.

Session affinity is an invariant enforced at the authenticated adapter/session
boundary for the load and flush operations, not a fifth public state
operation. The store never exposes, stores, or chooses a `PrincipalId`. The
authenticated transport supplies the principal to the server session, as
required by work ADR 0061; the opaque binding only rejects a different
authenticated session.

Loaded values are keyed by the complete client state key. Duplicate keys,
invalid key text, mismatched result batches, and impossible revision
transitions fail closed. A load replaces the matching USER value and revision
only after the complete batch passes validation.

A write result with `Written` advances the local revision and clears the dirty
flag. A `Conflict` does not overwrite the local value. It returns a typed
client error with the current server revision so the caller can reload and
reconcile explicitly. The first write for a missing cell uses
`expected_revision = None`; later writes use the loaded or acknowledged
revision.

## Evaluation and defaults

The evaluator receives a root state-session context containing the root
function, profile, and mounted instance key. Existing evaluator entry points
keep their current default context: the target function, the default profile,
and the default instance. New callers can provide an explicit context.

USER slots no longer fail merely because their scope is USER. A loaded USER
value is checked against the declared slot type and is used in the same way as
an in-memory state value. If no persisted value exists, the checked plan
default is evaluated locally. Evaluating a default does not create a durable
write. A caller must make an explicit state update and flush it through the
protected service.

This ADR does not add state assignment syntax. It does not infer writes from a
return expression or from toolkit events. The client runtime or a later
procedural CLIENT slice must call the explicit update operation.

## Flush policy

The store does not start an unbounded background task and does not choose a
timer interval. The owning runtime coalesces dirty keys and calls the service
at one of the contract flush points:

- interaction completion;
- a bounded debounce interval;
- explicit save;
- clean invocation shutdown; or
- a bounded background batch.

One flush sends one root function and profile context with a bounded batch of
changes. The server keeps its existing atomic batch and per-cell optimistic
revision semantics from work ADR 0061.

## Alternatives considered

### Keep USER state rejected in the evaluator

This preserves the current fail-closed implementation, but it leaves the
accepted `sys.state.*` service disconnected from the CLIENT state model and
prevents an invocation from restoring saved preferences. Rejected because the
server lifecycle and the version-four plan metadata already exist.

### Store USER values as plain runtime values

This makes reads simple, but it loses the persisted `TypeId` and revision that
are required to construct safe writes and detect conflicts. Rejected because
it would turn a reload or write into an unchecked operation.

### Let the client select the principal

This would make local state APIs convenient, but it violates the authenticated
session boundary and permits cross-principal state access. Rejected because
the server must derive the principal from the session.

### Add a timer and transport task inside `ClientStateStore`

This would hide scheduling, cancellation, transport failure, and shutdown
ownership inside a value store. Rejected because the caller owns the runtime
lifecycle and must choose bounded flush points.

## Required implementation order

1. Add the instance-aware client key and root state-session context.
2. Store loaded USER values with type and revision metadata.
3. Add explicit load, update, pending-change, and write-result operations.
4. Permit USER values and defaults in the evaluator while retaining type and
   revision checks.
5. Add an authenticated adapter that establishes the opaque session binding
   before calling `PostgresKernel::load_user_state` and
   `PostgresKernel::write_user_state`, rejects a store bound to a different
   session, and never accepts a principal from client data.
6. Add a live proof for initial load, explicit update, flush, reload, and
   revision conflict.

Each implementation commit changes one to three files and keeps the workspace
buildable.

## Deferred surface

State assignment syntax, automatic toolkit-event capture, structured merge
functions, cross-principal inspection, non-default profile policy, dynamic
child call-site identity derivation, async resources, streams, and `AWAIT`
remain outside this ADR.

## Precedence

This decision extends work ADRs 0061 and 0069. Work ADR 0061 remains the
authority for the protected server service, principal derivation, typed values,
and optimistic revision outcomes. Work ADR 0069 remains the authority for
CLIENT state declaration syntax and version-four plan metadata. Spec ADR 0007
and `spec/docs/16-state-model.md` remain authoritative outside this
implementation scope. Work ADRs 0020 and 0023 remain authoritative for
authenticated session establishment; this decision adds only the opaque
session-affinity invariant at the client state boundary.
