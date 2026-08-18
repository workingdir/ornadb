# ADR 0076: Headless Runtime ABI Conformance Boundary

**Status:** Proposed

## Decision

Define a test-only, in-process headless runtime fixture as the first executable
runtime boundary. The fixture consumes the provisional `ORNA-UI/1` value frame
from work ADR 0062 and exercises the C-shaped lifecycle in
`spec/spec/orna_runtime_abi_v1.h`, but it does not accept a native toolkit,
load a shared library, open a database connection, authenticate, call a SERVER
function, or become a production runtime.

This is a contract proposal for the deferred candidate in
`spec/docs/49-gap-research-and-contract-plan.md:46-54`. It does not promote the
canonical runtime API from `CURRENT PROPOSAL` to an accepted specification.
The canonical specification remains authoritative until a corresponding spec
ADR accepts the ABI.

The smallest reversible implementation is a `#[cfg(test)]` conformance module
in `crates/orna-client`. It owns a fake descriptor, a C-shaped fixture table,
fixture loader, runtime state machine, callback log, and release counters. The
fixture table must use `#[repr(C)]` with the exact field order, integer widths,
pointer mutability, and function-pointer signatures of the header's
`OrnaRuntimeApiV1` and related structs. Compile-time size, alignment, and field
offset assertions are required. If Rust cannot provide that exact mirror, use
a tiny C fixture compiled against the header and an explicit test bridge rather
than a Rust-shaped substitute. The loader accepts the exact fixture table and
calls its `describe` entry point deterministically. It uses existing Rust
ownership and does not expose a new public runtime API.

## Contract proposed for acceptance

### Identity and compatibility

The fixture descriptor uses these test-only values:

| Item | Value |
| --- | --- |
| Runtime name | `orna-runtime-headless-conformance` |
| Runtime version | `1.0.0` |
| ABI major | `1` |
| ABI minor | `0` |
| Sink | `std.ui.UI` |
| Value frame | `ORNA-UI/1` |
| Thread model | caller pumps |

The fixture loader validates the complete descriptor before it creates runtime
state:

- `abi_major` must equal the supported major;
- `abi_minor` must not exceed the supported minor;
- runtime identity, platform, sink type, and contract names must be non-empty;
- contract and feature lists must not contain duplicate entries;
- required sink and contract versions must match exactly;
- unknown feature bits fail closed rather than being silently ignored;
- zero or malformed handles and invalid descriptor counts are rejected.

The loader is an in-process test fixture. It is not a `dlopen` implementation
and does not define shared-library discovery or trust policy. A successful
fixture load proves descriptor and lifecycle validation only; it does not prove
that an operating system can load a production shared library.

### Representation and ownership

The headless proof uses the existing `ORNA-UI/1` canonical JSON bytes as an
immutable value reference. The client owns the source value and borrows its
pointer and length only for the containing call or callback. The fixture copies
bytes before it retains them. It never retains descriptor arrays, string views,
operation arrays, event pointers, or callback user data after their owning call
returns.

`OrnaOwnedBytes` output follows one transfer rule:

- a successful capture returns a non-null pointer when `len > 0`;
- an empty result uses a null pointer and zero length;
- the receiver calls the supplied release callback exactly once;
- a second release, a mismatched owner, or a changed length is a fixture error;
- failed operations do not transfer an output buffer.

This closes ownership for the test fixture only. It does not replace the open
production `OrnaValueRefV1` choice between handles and encoded slices.

### Handles and lifetime

The fixture allocates non-zero handles monotonically within one runtime. Every
operation checks the handle provenance and current lifecycle state. Destroying
a surface invalidates its node, action, model, and request handles. Runtime
shutdown invalidates all remaining handles. A stale or foreign handle returns a
stable `NOT_FOUND` or `INVALID_ARGUMENT` status and cannot mutate state.

The lifecycle is:

```text
query and validate descriptor
    -> create runtime
    -> create surface
    -> apply UI batches and report events
    -> destroy surface
    -> request shutdown
    -> drain caller-pumped work
    -> destroy runtime
```

The fixture rejects operations after terminal shutdown. Runtime destruction is
valid only after shutdown is terminal and all surfaces are destroyed.

### Thread and callback rules

The fixture uses the caller-pumps thread model. The thread that creates the
runtime owns all runtime calls. Calls from another thread return `BUSY` and are
not queued implicitly.

Callbacks run on one FIFO serial lane. The fixture rejects synchronous
re-entry from a callback with `BUSY`; it does not call the database or a SERVER
function from a callback. No callback overlaps another callback, and no
callback runs after terminal shutdown. The callback log records sequence and
terminal state so tests can prove these rules without a native event loop.

### Atomic batches and semantic revisions

A surface starts at semantic revision `0`. A non-empty UI batch is valid only
when its revision equals the current revision plus one. The fixture rejects a
stale revision with a dedicated `STALE_REVISION` status and rejects a revision
gap with `INVALID_ARGUMENT`.

The fixture validates every operation, handle, parent, slot, ordinal, property,
and value reference before it changes the surface. A failed batch changes
neither the semantic tree nor its revision. A successful batch commits all
operations together and updates the revision once. Empty batches are invalid so
that a no-op cannot advance the revision.

Semantic state capture returns a deterministic canonical byte sequence through
`OrnaOwnedBytes`. The captured state is unchanged after a rejected batch.

### Typed events and model requests

The fixture accepts the event kinds already listed in the header:

- action;
- focus changed;
- layout state changed;
- surface closed;
- model range request;
- model children request;
- diagnostic.

Each event must refer to a live surface and valid node, action, model, or
request handle where the event kind requires one. The fixture records typed
payload tags and rejects a payload with the wrong kind or provenance.

Each model request has one terminal outcome. A completion, failure, or
cancellation wins the request race exactly once. Cancellation wins when it is
observed before a terminal completion. A late row result or second terminal
outcome returns `CANCELLED` or `NOT_FOUND` and cannot call the client callback.

### Shutdown and failure

`request_shutdown` rejects new surfaces and requests, cancels each outstanding
model request exactly once, emits the required terminal surface events, drains
all queued callbacks, and then enters terminal state. No callback occurs after
that state. Failure messages contain status and a stable diagnostic code but do
not contain authentication credentials, principals, raw request arguments, or
opaque value bytes.

The fixture reports validation, stale revision, cancellation, and lifecycle
errors through stable structured values. It does not invent a public protocol
mapping for errors that the canonical ABI has not accepted.

## Focused proof matrix

The test-only module must prove at least:

1. a valid C-shaped fixture table loads and an incompatible ABI major is
   rejected before `describe` can create runtime state;
2. duplicate contracts, unsupported versions, malformed counts, and unknown
   required features fail closed;
3. borrowed input is valid during a call but is not retained after return;
4. captured bytes transfer to the caller and release exactly once;
5. foreign and stale handles cannot mutate a surface;
6. valid batches commit atomically, malformed batches leave no partial change,
   stale revisions are rejected, and the captured state is deterministic;
7. callback order is FIFO, callbacks do not overlap, and re-entry is rejected;
8. typed events preserve handle provenance and model requests complete once;
9. cancellation rejects late completion and shutdown cancels outstanding work;
10. the end-to-end fixture proof reaches terminal shutdown with no post-terminal
    callback.

The C header syntax check remains a separate gate:

```bash
gcc -std=c11 -fsyntax-only spec/orna_runtime_abi_v1.h
```

The Rust proof runs through the normal package test command with the focused
runtime-conformance filter. No native runtime or live PostgreSQL proof is part
of this first slice.

## Implementation plan after acceptance

1. Accept the canonical runtime ABI contract in the spec repository, add the
   `STALE_REVISION` status and any missing event payload fields, and keep the
   header and API pages consistent. Do not change the provisional UI value
   codec in ADR 0062 unless the accepted ABI explicitly replaces it.
2. Update this work ADR to `Accepted` and add its implementation order to the
   work decision index.
3. Add one `#[cfg(test)]` module in `crates/orna-client/src/lib.rs` or one
   private test module beside it. Reuse `RuntimeValue`, `RevisionPair`, the
   existing canonical value codec, and the deterministic resource lifecycle
   patterns. Do not modify `orna-server`, `orna-protocol`, or
   `orna-system-tests` for the first proof.
4. Add the focused descriptor, ownership, batch, event, cancellation, and
   shutdown tests, then run the C syntax check, the focused `orna-client`
   tests, `cargo check --workspace --all-targets`, and the workspace gate.

Each implementation increment must change one to three files and keep the
workspace buildable. A later production runtime ADR may reuse the conformance
fixture, but it must separately accept shared-library loading, platform
selection, event-loop integration, native ownership, and toolkit-specific
behaviour.

## Alternatives considered

### Implement Qt, GTK, or Web first

This would force unresolved allocator, thread, callback, shutdown, and event
semantics into a toolkit adapter. It would also add native deployment and
platform failures before the semantic contract is testable. Rejected until the
headless contract is accepted.

### Add a public runtime loader now

The current specification does not define trust, discovery, library lifetime,
or production error mapping. A public loader would make proposal-level fields
look stable and would be difficult to remove. Rejected in favour of a private
fixture loader in tests.

### Use the existing CLIENT resource executor as the runtime boundary

The resource executor proves typed request and completion identity, but it has
no surface, semantic tree, event, or runtime ownership model. It is useful as a
pattern and fixture source, not as an ABI substitute.

### Treat the C header as already accepted

The header is explicitly a design draft and leaves the value representation,
ownership, thread rules, re-entrancy, cancellation, and shutdown ordering open.
Using it as a production contract would violate the source-of-truth status.
Rejected.

## Deferred surface

Native runtime libraries, automatic platform selection, shared-library trust
and discovery, graphical widgets, accessibility bridges, browser deployment,
production event-loop integration, and runtime-specific opaque layout state
remain outside this proposal. CLIENT-to-SERVER resources and reflective
protocol gateways remain governed by their own contract work.

## Precedence

The canonical specification and accepted spec ADRs remain authoritative. Work
ADRs 0062 and 0063 remain authoritative for the provisional `std.ui.UI` value
and TTY runtime selection. Work ADR 0074 remains authoritative for the
executor-independent CLIENT resource seam. This proposal narrows only the
shape of a future test-only conformance slice.
