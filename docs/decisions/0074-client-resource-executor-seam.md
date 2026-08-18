# ADR 0074: Runtime-Only CLIENT Resource Executor Seam

**Status:** Accepted

## Decision

OrnaDB adds a runtime-only executor seam for the resource lifecycle defined by
work ADR 0071. This slice does not add `RESOURCE`, `AWAIT`, action, stream, or
assignment syntax. It does not define a transport protocol or start background
work.

The local CLIENT runtime exposes three closed contracts:

* `ClientResourceRequest` carries the complete `ClientResourceKey`, the request
generation, the expected resolved result type, and the typed function
arguments.
* `ClientResourceCompletion` carries the same resource key and generation with
either one runtime value or one structured failure code.
* `ClientResourceExecutor` accepts one request and returns one completion. The
executor owns the request after submission. The runtime applies the completion
through the resource's existing type, active-revision, generation, and state
checks.

A request starts only after the runtime validates the complete target against
the active revision and calculates the canonical typed argument digest. The
digest is domain-separated and contains arguments sorted by ascending
`ParameterId`, with each argument represented by its canonical active ORV3
`CallArgument` frame and a length prefix. Duplicate parameter identities and
values that the active codec cannot encode are rejected before the resource
enters `LOADING`. The calculated digest must equal the digest in the resource
key.

Applying a completion first checks that its key is the resource key. The
existing generation and lifecycle checks then reject stale, cancelled, or
otherwise invalid completions without changing the resource. A ready
completion also uses the existing active catalogue and expected-type checks.

The crate provides a deterministic immediate executor adapter for host glue and
focused tests. It invokes a caller-supplied closure in the current call and
turns its `Result<RuntimeValue, String>` into a completion. The adapter does not
select a scheduler, open PostgreSQL, perform transport, retry, or hide
cancellation policy.

## Rationale

ADR 0071 deliberately stops at a deterministic resource state machine because
source syntax, transport, scheduling, and backpressure remain open in the
canonical specification. A small typed request/completion boundary lets a
future executor integrate without passing untyped values or bypassing the
resource's principal, revision, argument, and invalidation identity.

Canonical argument bytes are derived from the existing active ORV3 codec rather
than from `Debug` output or a second value encoder. Sorting by parameter
identity makes equivalent argument sets share one cache identity even when a
caller provides them in a different order.

The immediate adapter is intentionally not an asynchronous implementation. It
is a deterministic seam for a later runtime or transport adapter, and keeping
it synchronous prevents this ADR from silently deciding event-loop ownership.

## Alternatives considered

### Add `RESOURCE` and `AWAIT` syntax now

This would make the feature user-visible, but the canonical specification marks
those forms and their failure, stream, and cancellation semantics as open. It
would combine language, runtime, and transport decisions in one change.
Rejected until those contracts receive a separate accepted ADR.

### Put transport and scheduling in `ClientResource`

This would couple cache state to one executor and make shutdown,
cancellation, retry, and backpressure implicit. Rejected because the CLIENT
runtime owns those policies.

### Hash argument debug text

Debug output is not a stable value contract and can change without a semantic
change. Rejected in favour of the existing canonical active value codec.

### Accept completions by generation only

A generation alone does not identify the target, principal, arguments, or
invalidation epoch. Rejected because a completion from another resource could
otherwise reach the wrong state object.

## Required implementation order

1. Add canonical argument digest validation and the request/completion types to
   `orna-client`.
2. Add completion application and the deterministic immediate adapter.
3. Prove reordered arguments, digest mismatch, cancellation, stale
   completions, failure, and successful typed publication.
4. Keep source syntax, transport, and asynchronous executor integration
   deferred to a later ADR.

Each implementation commit changes one to three files and keeps the workspace
buildable.

## Deferred surface

`RESOURCE` declarations, `AWAIT`, actions, streams, retry and cache policy,
transport framing, server resource execution, scheduler selection, event-loop
integration, and runtime-specific cancellation remain outside this ADR.

## Precedence

This decision extends work ADR 0071 only with a runtime request/completion seam.
ADR 0071 remains authoritative for resource identity and lifecycle semantics.
Work ADRs 0060, 0068, 0069, 0070, and 0073 remain authoritative for capability,
expression, state, USER state, and SET transport behaviour. The canonical
specification remains authoritative outside this accepted implementation scope.
