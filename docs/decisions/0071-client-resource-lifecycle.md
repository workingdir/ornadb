# ADR 0071: CLIENT Resource Lifecycle

**Status:** Accepted

**Implementation status:** The executor-independent resource identity and
lifecycle described here are implemented. The formerly open resource language,
transport, and action portions are accepted successors in work ADRs 0077, 0078,
and 0079; this ADR remains the authority for cache identity and lifecycle
transitions. The security-cache correction is tracked under Beads `ornadb-r4o`;
this note records the contract, not live proof.

## Decision

The first CLIENT resource slice is an executor-independent lifecycle and cache
identity model. It did not add `RESOURCE`, `AWAIT`, action, stream, or
assignment syntax; those forms were open at the time and are now defined by
work ADRs 0077-0079.

A resource is owned by the CLIENT runtime and represents one typed asynchronous
request. Its identity contains the complete cache boundary:

```text
target FunctionId + pinned revision
principal/security context, including immutable authorization/security-snapshot
epoch evidence
canonical typed argument digest
catalogue/data invalidation token
```

The immutable security-snapshot digest is local cache-identity evidence. The
trusted invocation boundary derives it; callers cannot select it, and it is
never sent in resource transport or audit payloads.

The runtime stores this identity with one expected resolved type and one
explicit state machine:

```text
IDLE -> LOADING -> READY
                 -> FAILED
                 -> CANCELLED
```

A new request generation invalidates the previous generation. Completion,
failure, and cancellation messages must carry the generation that created them.
A message for an older generation is rejected and cannot overwrite the current
value. `READY` stores one typed `RuntimeValue`; `FAILED` stores one structured
failure code; `CANCELLED` stores neither.

The resource validates a successful value against its expected resolved type
and the active catalogue before it changes state. The resource does not start a
background task, select a debounce interval, call PostgreSQL, or choose a wire
protocol. The owning CLIENT runtime remains responsible for scheduling,
transport, backpressure, cancellation, and rerender requests.

## Rationale

The canonical resource proposal requires typed values, dependency-aware
invalidation, cancellation, and principal-safe cache identity, but leaves the
source syntax and transport contract open. A small state model gives later
executors one fail-closed boundary without inventing a public language form or
hiding scheduling inside a value store.

Generation ownership is used instead of timestamps or task handles. It is
deterministic, cheap to compare, and works for local, threaded, and event-loop
executors. The identity keeps principal and revision facts explicit so a later
cache cannot accidentally reuse a result across users, revisions, or
invalidation epochs.

## Alternatives considered

### Add the proposed resource syntax now

This would make the feature visible, but the specification marks the exact
syntax as open. It would also require decisions about futures, action values,
stream ownership, and parser recovery in one change. Rejected until those
contracts are accepted.

### Put transport and scheduling in `ClientResource`

This would make a first demo easier, but it would couple the value model to one
executor and obscure shutdown, cancellation, retry, and backpressure ownership.
Rejected because the runtime owns those policies.

### Use a timestamp or task handle for stale completion checks

Timestamps are not deterministic and task handles are executor-specific. A
monotonic generation is sufficient for the contract and remains portable.
Rejected.

### Cache only by target and arguments

This can return a value produced for another principal, revision, or data
snapshot. Rejected because the canonical cache identity includes all four
boundaries.

## Required implementation order

1. Add the closed identity, status, failure, and generation types to the local
   CLIENT runtime.
2. Validate successful values against the expected type before publication.
3. Reject stale and invalid transitions without changing the current state.
4. Add focused lifecycle tests for refresh, success, failure, cancellation,
   invalidation, type mismatch, and stale completions.
5. Add an executor and transport only after the resource/action/stream syntax
   and wire contract are accepted by a later ADR.

Each implementation commit changes one to three files and keeps the workspace
buildable.

## Deferred surface

Virtual models, retry and cache policy, and production runtime event-loop
integration remain outside this ADR. The accepted `RESOURCE`/`AWAIT` language,
stream transport, server resource execution, and executable action slice are
defined by work ADRs 0077, 0078, and 0079.

## Precedence

This decision narrows the current proposal in
`spec/docs/21-resources-actions-streams.md` without changing the canonical
specification. Work ADRs 0060, 0068, 0069, and 0070 remain authoritative for
capability, expression, state declaration, and USER state semantics. Work ADRs
0077-0079 define the accepted language, transport, and action successors to the
open portions recorded here.
