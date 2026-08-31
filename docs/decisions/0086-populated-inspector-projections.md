# ADR 0086: Populate the Existing Inspector Projection Rows

**Status:** Accepted

**Scope:** This is a work ADR for the next headless Inspector implementation
slice. It accepts bounded population of the already-registered `@1` projection
row types. It does not claim that the Inspector, Studio, or any graphical
runtime is complete.

## Context

Work ADR 0064 accepted the server-side `sys.inspect` snapshot epoch, eight
materialized projections, the privilege ladder, and the trace model. Work ADR
0080 accepted the ordinary CLIENT Inspector and its immutable, transient,
redacted carriers. Work ADR 0081 made `std.inspect.render@1` the generic
contract for the nine-carrier renderer, without changing the carrier model.

The accepted implementation now captures an immutable epoch, but the current
headless path leaves the `resources`, `ui_nodes`, and
`presentation_candidates` vectors empty. `runtime_bindings` is already derived
from the client offer, and `state_cells` is already derived from the exact
state cells loaded for the invocation. This leaves a real but unnecessarily
sparse Inspector result: the existing row types and their encoders already
have a closed shape, while their safe capture sources have not yet been wired
into the epoch builder.

The relevant existing facts are deliberately narrow:

* `InvocationEventBatch` is a non-empty, one-invocation, contiguous event
  sequence. Its sealed v1 bodies are `Started`, `ValueBatch`, `Diagnostic`,
  `Completed`, `Failed`, and `Cancelled`.
* `InvocationClientOffer` contains the checked sink and runtime offers. A
  runtime offer contains its name, version, consumed type descriptors,
  contract `(name, version, features)` values, signed preference rank, trust
  fact, and optional limits. The canonical request codec normalises offer
  lists before dispatch; `InvocationClientOffer` exposes the resulting order.
* A CLIENT dispatch loads USER state in the protected repeatable-read
  transaction. The exact loaded `UserStateCell` values are retained for the
  capture call, including the retry path; the capture code must not read live
  USER state again after that point.
* The sealed presenter path resolves the existing presenter registry using
  alias, media type, or type-name precedence, checks streaming and client sink
  compatibility, and records the final presented value in the event batch.
  The registry is deterministic (priority descending, alias ascending), but
  the v1 row type does not carry scores or the complete candidate search.
* The `std.ui.UI` value is the validated `ORNA-UI/1` semantic value. Its node
  schema has a contract identity and optional `call_site_id` and
  `function_instance_id`, but it also has properties, slots, actions, keys,
  and source-origin data that this decision does not project.

The safe design is therefore to populate only facts already present at the
protected capture boundary. No projection is allowed to query the current
catalogue, current USER state, a runtime process, a toolkit, or a mutable
resource cache after the epoch is captured.

## Decision

### 1. Keep the canonical `@1` identities and frame

Population keeps the canonical `@1` identities and frame fields. For this
release, the canonical `ORNA-INSPECT/1` wire layout uses the complete
kernel-generated `InspectEpochId` as an exact 16-byte value. An earlier
low-`u64` implementation or draft was nonconforming and is not a supported
wire variant; readers and writers must use the canonical layout together.
* `ORNA-INSPECT/1 ` magic and carrier version `u16 = 1`;
* carrier tag `1` for the snapshot and tags `2` through `9` for invocation
  nodes, calls, resources, state cells, UI nodes, presentation candidates,
  runtime bindings, and security decisions respectively;
* the snapshot and projection `FunctionId`s `...03` through `...0b`, with
  `sys.inspect.trace` still at `...0c`;
* sealed carrier `TypeId`s `...f3` through `...f6` for invocation, snapshot,
  snapshot options, and trace event, `...f7` for
  `sys.security.principal`, and `...f8` through `...ff` for the eight
  projection result carriers;
* representation contracts `orna.sys.inspect.*@1`;
* the envelope order: magic, carrier version, projection tag, complete
  kernel-generated `InspectEpochId` (exact 16-byte identity in network byte
  order), source revision, catalogue revision, bounded row count, then
  length-delimited row frames; and
* the exact nine-parameter `std.inspect.render@1` order: snapshot,
  invocation nodes, calls, resources, state cells, UI nodes, presentation
  candidates, runtime bindings, and security decisions.

`InspectCarrierEnvelope`, its ORV5 row wrapper, `make_inspect_carrier`, the
existing projection encoders, and the existing decoder remain the framing
authority. New rows are passed through those paths; no second JSON or private
wire representation is introduced.

The persisted `INEP` epoch envelope also remains version 1 and retains its
existing collection order: summary, invocation nodes, calls, resources, state
cells, UI nodes, presentation candidates, runtime bindings, and security
decisions. Existing epochs with empty vectors remain valid and immutable.

### 2. Preserve the exact bounded row schemas

The row structs in `orna-core/src/inspect.rs` are the accepted v1 shape. The
population slice may construct more instances of these structs, but it must
not widen them or reinterpret an existing field.

#### `ResourceRow`

```text
kind   = State | Catalog | Standard | Runtime
status = Active | Invalidated | Released
```

This is a category/status observation only. It has no resource request
identity, target, revision, principal, argument digest, invalidation token,
generation, typed result, stream state, or failure payload.

The narrow capture algorithm emits at most one row for each category:

1. emit `State / Active` once the exact state context has been loaded for the
   capture (an empty loaded cell set still proves a successful state load);
2. emit `Catalog / Active` for the active application catalogue revision
   pinned by the epoch;
3. emit `Standard / Active` when the verified standard snapshot required by
   the sealed invocation and its registered codecs is present; and
4. emit `Runtime / Active` when the trusted capture input contains at least
   one client runtime offer.

A missing runtime offer produces no Runtime row. It does not get translated to
`Released` or `Invalidated`. The current capture inputs contain no immutable
resource lifecycle evidence for those statuses, so this slice emits neither
status. Those enum values remain available to a later resource-lifecycle
contract. Absence is not a live cache lookup and is not a synthetic failure
state.

The state row is sourced from the same exact loaded-state fact that feeds the
existing `state_cells` rows. For a path that has not supplied loaded cells,
state is loaded inside the existing capture transaction before the row is
constructed. The catalogue and standard rows are copied from the already
pinned active revision; they are not looked up during a later projection call.

#### `UiNodeRow`

```text
function          FunctionId
call_site         non-empty canonical semantic call-site label
runtime_contract  non-empty canonical runtime-contract identity
```

For each node in a validated `std.ui.UI` value present in the captured client
`ValueBatch`, the extractor may emit one row containing only these identities:

* `function` is the checked owning CLIENT `FunctionId` from the captured
  execution context. The root target identity is the owner for the returned
  semantic tree unless the already-checked client execution supplies a nested
  `FunctionId` explicitly.
* `call_site` is the canonical textual form of the node's checked
  `call_site_id`. A node whose optional call-site identity is absent is not
  given a made-up call site and is omitted from this identity-only projection.
  An invalid or empty supplied label is a malformed capture, not a label to be
  repaired.
* `runtime_contract` is the node's canonical contract identity (`id` in the
  `ORNA-UI/1` contract object), retaining the name/version identity already
  validated by the UI codec.

`function_instance_id`, UI keys, parent and ordinal relationships,
`source_origin`, properties, slots, and actions are not copied into this row.
A non-`FunctionId` instance string is never coerced into a function identity.
A valid UI value can consequently yield no row when its optional semantic
call-site identity is absent; that is an intentional fail-closed omission,
not permission to invent identity.

The node walk is bounded by the existing `MAX_RUNTIME_VALUE_NODES` UI/value
limit. It consumes the captured canonical `ORNA-UI/1` bytes and does not ask a
runtime or toolkit to inspect the target UI.

#### `PresentationCandidateRow`

```text
presenter       non-empty presenter alias
accepted        boolean
reason          non-empty bounded public reason
selected_sink   optional TypeDescriptor
runtime         optional non-empty selected runtime name
```

For a successfully captured invocation with an output requirement, the row is
the final presenter decision already made by the sealed dispatch path:

* `presenter` is the existing deterministic presenter alias;
* `accepted` is `true`;
* `reason` is the existing bounded reason, currently
  `accepted by output resolution`;
* `selected_sink`, when the dispatch evidence identifies one, is the matching
  offered sink descriptor copied from `InvocationSinkOffer::descriptor`; and
* `runtime`, when the trusted local client selection identifies one, is the
  selected `InvocationRuntimeOffer` name.

The row is therefore zero or one row for the current sealed route. An
invocation with no output requirement has no presentation decision and emits
no row. A failed or unresolved presentation retains the existing
`PresentationFailed`/no-epoch behavior; it does not fabricate a presenter
alias merely to create a rejected row. A future contract may retain all
rejected candidates, scores, or selector details, but this ADR does not add
those semantics.

The `reason` is a stable public outcome label, never a raw alias selector,
argument, result, policy input, or engine diagnostic. The selected sink and
runtime are the dispatch facts, not a fresh resolution against the current
client or catalogue.

#### `RuntimeBindingRow`

```text
runtime_name          non-empty runtime name
version               non-empty runtime version
consumed_descriptors  ordered TypeDescriptor list
contracts             ordered (name, version, features) list
trusted               local installation trust fact
preference_rank       u32 rank
```

There is one row for each `InvocationRuntimeOffer` in the canonical decoded
client offer. The row copies the existing offer projection exactly:

* name and version come from the offer;
* consumed descriptors are copied in the offer's canonical descriptor order;
* each contract retains its name, version, and feature list;
* `trusted` is copied unchanged; and
* the signed `i32` preference rank is mapped using the existing rule
  `max(0) as u32`, so a de-prioritised negative rank is represented by rank
  zero.

The row deliberately omits offer limits, ABI/library paths, platform details,
thread models, and any selection authority because the existing row type does
not contain them. An empty client runtime-offer list remains an empty
`runtime_bindings` projection.

### 3. Use one deterministic capture boundary

Population occurs while the existing protected invocation capture owns its
repeatable-read transaction and its active `RevisionPair`. The capture
inputs are:

```text
captured InvocationEventBatch
captured presenter outcome/requirement facts, when presentation was used
canonical InvocationClientOffer, when one exists
exact loaded USER state cells (or the same-transaction state load)
active source/catalogue revision pair and verified standard snapshot
```

The implementation constructs all projection vectors before inserting the
immutable epoch. It copies values into the epoch; it does not retain borrowed
client-offer, event, state-store, or runtime references. Once
`InspectSnapshotEpoch` has been created and `summary_bytes` committed, every
projection accessor reads only that epoch payload.

The source vectors use the repository's existing canonical order. In
particular:

1. event-derived facts use the retained `InvocationEventBatch` order and its
   contiguous event sequence;
2. runtime offers and their nested descriptors/contracts/features use the
   canonical order established by the existing invocation carrier decoder;
3. UI nodes use the validated semantic UI traversal order; and
4. presentation facts use the deterministic presenter dispatch result and the
   canonical selected offer order.

Before a carrier is emitted, the existing projection encoders retain their
field order and `make_inspect_carrier` retains its `row(tag, index)` ordinal,
common provenance prefix, ORV5 list wrapper, and strict encoded-row sort. No
new semantic sort key is introduced. Any duplicate or out-of-order complete
row frame rejected by the current carrier validator remains an error.

The result is one immutable server epoch. A later source/catalogue revision,
state write, runtime offer, presenter registry change, or resource completion
cannot alter it and cannot be observed by re-running a projection accessor.

### 4. Keep ownership, privileges, and redaction unchanged

The kernel/server owns the persisted epoch and its canonical `summary_bytes`.
The authenticated session and its effective `InspectPrivilege` set are still
required before a snapshot or projection is returned. The client owns the
transient projection carriers and its separate client execution epoch. A
carrier, row, or renderer result is never an authority token and cannot be
persisted in USER state or sent through a SERVER resource.

The existing scope ladder remains `OwnInvocation`, `SessionInvocations`, and
`AnyInvocation`; `Values`, `Source`, `SecurityDetails`, and
`RuntimeInternals` remain independent classifiers. Population does not make a
new grant implicit:

| Row/fact | Existing visibility rule |
| --- | --- |
| Resource kind/status | Structural; no classified value is added. |
| UI `function` | Structural function identity. |
| UI `call_site` | Redacted unless `Source` is granted. |
| UI `runtime_contract` | Redacted unless `RuntimeInternals` is granted. |
| Presentation `accepted` | Structural boolean. |
| Presentation presenter/reason/sink/runtime | Redacted unless `RuntimeInternals` is granted. |
| Runtime name/version | Existing structural labels remain visible. |
| Runtime descriptors/contracts/trust/rank | Redacted unless `RuntimeInternals` is granted. |
| State-cell typed values and call schemas | Existing `Values` behavior is unchanged. |

The carrier continues to encode typed redaction markers and classifier
evidence through the existing path. This ADR does not introduce richer
per-field redaction. No row may contain a credential, principal or grant
contents, `run_as` authority, raw secret, raw argument, unbounded source text,
opaque bytes without a registered codec, native handle, or runtime library
path.

### 5. Keep epoch, observer, freeze, resume, and recursion rules unchanged

The captured server epoch remains bound to:

```text
server InspectEpochId
target InvocationId
captured source/catalogue RevisionPair
authenticated owner scope
observer lineage and purpose
capture classifier policy
```

The client binding remains:

```text
client_epoch_id -> server_epoch_id -> captured RevisionPair
```

The existing client lifecycle checks exact target, observer root and parent,
principal, revision, projection-version, and generation evidence. Freeze and
resume accept only the exact live token in the same CLIENT execution. Refresh
creates a new snapshot; it does not mutate the frozen one. Cancellation,
client shutdown, principal change, or active-revision replacement invalidates
pending operations and prevents a late result from publishing into a later
epoch.

Observer identity is supplied from trusted execution context, never accepted
as caller authority. The default suppression still removes effects caused by
the observing invocation. Inspecting the observer root or any protected
server-recorded descendant fails with `inspect.recursion` before the target is
executed or a new projection is published. Row population is pure capture work
and must not create an additional inspection invocation or feedback loop.

### 6. Fail closed at every existing boundary

The following rules are part of the accepted contract:

* A row count above `MAX_INSPECT_CARRIER_ROWS` (65,536), a row above
  `MAX_INSPECT_CARRIER_ROW_BYTES` (16 MiB), an envelope above
  `MAX_INSPECT_CARRIER_BYTES` (16 MiB), an ORV5/opaque payload above its
  existing 16 MiB bound, a UI/value node count above
  `MAX_RUNTIME_VALUE_NODES` (65,536), or an existing text/descriptor/nested
  value bound fails with `inspect.limit` at the public carrier boundary.
  Counts are checked before attacker-controlled allocation. The epoch insert
  and carrier publication are atomic; no partial rows are returned.
* Malformed `ORNA-UI/1`, ORV5, typed descriptor, event, offer, row, or epoch
  data fails closed as `inspect.malformed_carrier` or the existing protected
  `inspect.projection_failed` mapping. Unknown registered-carrier identities
  remain `inspect.unknown_carrier`. Trailing bytes, invalid enum values,
  missing required labels, duplicate/non-canonical rows, and inconsistent
  epoch evidence are not repaired.
* A stale, future, foreign, target-mismatched, principal-mismatched, or
  revision-mismatched epoch/carrier/token fails with the existing
  `inspect.stale_epoch`, `inspect.future_epoch`, or `inspect.epoch_mismatch`
  code as applicable. A scope denial remains `inspect.denied`.
* Cancellation and completed-client lifetime failures remain
  `inspect.cancelled` and `inspect.closed`; absence of the required headless
  provider remains `inspect.runtime_unavailable`.
* An invalid optional UI call-site is omitted only when it is absent and
  therefore cannot be represented by `UiNodeRow`. An invalid present value,
  invalid contract identity, event inconsistency, state revision mismatch,
  or failed checked row construction fails the capture; it is never replaced
  by a placeholder.
* A stale resource generation or late resource completion cannot update an
  already captured epoch. Resource request identities and lifecycle events are
  not smuggled into `ResourceRow` to make that state observable.

Errors remain terminal for the affected operation, carry stable public codes,
and cannot overwrite a later generation. Retries allocate a new request and
repeat provenance and authorization checks.

## Compatibility

This release freezes the canonical `ORNA-INSPECT/1` wire layout. The canonical
epoch field is the complete 16-byte `InspectEpochId`; an earlier low-`u64`
implementation or draft was pre-release and nonconforming, not a supported
wire variant. There is no mixed-width compatibility mode, so readers and
writers must be upgraded together.

* Canonical `ORNA-INSPECT/1` decoders continue to decode old empty projections
  and newly populated rows using the same tag, field, provenance, and ORV5
  framing. Existing canonical persisted epochs are not rewritten.
* A pre-release decoder that expects only the low eight epoch bytes cannot
  decode canonical carriers; it must be upgraded before deployment. This
  coordinated implementation change does not introduce a second carrier
  version because carrier version `1` is the frozen canonical contract.
* Existing snapshot, projection, security-decision, state-cell, and trace
  identities remain stable. `sys.inspect.trace` remains the existing separate
  model-expressible trace API; this ADR does not make projection rows into a
  stream.
* The exact generic renderer remains `std.inspect.render@1`. Historical
  `devtools.inspector_shell@1` decoding/compatibility behavior remains the
  explicit policy of ADR 0081; this ADR adds no alias or reinterpretation.
* Existing headless runtime behavior, `std.ui.UI` validation, resource
  lifecycle validation, runtime offer selection, and authenticated sealed
  `sys.invoke` behavior remain authoritative. The projection observes those
  facts; it does not change their execution.
* No database migration is required: the existing inspection relations and
  `INEP` epoch payload are reused. Recovery must continue to round-trip both
  old empty vectors and new populated vectors canonically.

## Explicitly deferred

This ADR intentionally does **not** accept any of the following:

* resource request identities, target/revision/principal boundaries,
  canonical argument digests, invalidation tokens, generations, typed resource
  values, retry state, cancellation state, scalar/stream distinction, stream
  item rows, stream lifecycle rows, cursors, backpressure, or late-frame
  projections;
* UI properties, typed property values, slots, child ordinals, keys,
  function-instance IDs, parent relationships, actions, action payloads,
  source spans, layout/focus/accessibility data, reconciliation state, or any
  other full UI tree projection;
* native Qt, GTK, SwiftUI, ImGui, Web, or browser handles; runtime ABI
  ownership, event-loop, callback, loader, or platform semantics;
* model contracts, model range/child/completion requests, model handles, or
  collection virtualization;
* presenter scores, all rejected candidates, selector inputs, complete
  presenter registry diagnostics, or richer per-field redaction;
* trace streams beyond the already accepted `sys.inspect.trace` model,
  incremental projection updates, live refresh, SQL/network/runtime traces,
  or source editing/apply/hot reload;
* durable snapshot objects, persisted freeze tokens, cross-principal carrier
  reuse, caller-supplied authority, reflective gateways, JSON-RPC/MCP
  exposure, or Studio constructor/launch/database semantics.

In particular, accepting `UiNodeRow` identity extraction does not accept a Qt
or Studio constructor contract. Accepting runtime-offer projection does not
accept a new runtime family, a library path, or database-selected native code.
Accepting a bounded presenter result does not accept a gateway or a model
protocol.

## Alternatives considered

### Add a new `@2` projection carrier

Rejected. The existing `@1` row structs, ORNA-INSPECT envelope, TypeIds, and
renderer signature already express the accepted identity facts. A new carrier
would split decoder behavior and invalidate the compatibility and immutable
epoch guarantees without adding a required semantic field.

### Put full resource identity and lifecycle into `ResourceRow`

Rejected for this slice. ADRs 0071, 0077, and 0078 define resource identity,
generation, transport, and lifecycle, but `ResourceRow` intentionally contains
only kind and status. Adding request IDs, argument digests, invalidation
tokens, or stream rows would require a new row schema and a separate contract
for immutable lifecycle capture, redaction, and stale completion handling.
Those facts remain deferred rather than being truncated into a misleading
category row.

### Re-read live state, runtime, presenter, or UI data from each projection

Rejected. It would make one snapshot produce different answers over time,
allow a later revision or principal context to leak into an old epoch, and
break freeze/resume and stale-completion guarantees. All accepted rows are
copied at the existing protected capture boundary.

### Encode arbitrary UI JSON or native toolkit handles

Rejected. The canonical UI value is semantic and transient, while the row
shape intentionally exposes only function/call-site/runtime-contract identity.
Properties, slots, actions, instance handles, and toolkit state require a
separate UI tree/model contract. The headless runtime must never be promoted
by this ADR into a native runtime.

### Enumerate every presenter candidate and its score

Rejected for `@1`. The current sealed route records a deterministic final
presenter decision and its selected sink/runtime; the row type has no score or
selector field. A complete candidate search would require planner
instrumentation and a new bounded diagnostic contract. The accepted row is
zero or one final decision row and never invents a rejected alias after a
failed presentation that has no epoch.

### Turn the projections into live streams

Rejected. Work ADRs 0080 and 0081 deliberately use materialized bounded
carriers. Stream item identity, cursors, backpressure, cancellation, and
incremental redaction are separate decisions. `sys.inspect.trace` remains the
only distinct trace stream surface.

### Accept Qt, models, gateways, or Studio semantics at the same time

Rejected. Work ADRs 0082 through 0085 separately define the production Qt
runtime/package boundary, while ADR 0084 explicitly leaves models, gateways,
and populated Inspector rows separate. Combining those concerns would make a
small projection population change decide unrelated ownership, ABI,
authentication, and launch contracts.

## Implementation artifacts

The later implementation slice must reuse these existing artifacts:

```text
crates/orna-core/src/inspect.rs
    Existing @1 row structs, InspectSnapshotEpoch, privilege/classifier model,
    and immutable projection accessors.

crates/orna-core/src/inspect_carrier.rs
    ORNA-INSPECT/1 envelope, projection tags, bounds, ORV5 row validation,
    target-bound provenance, and strict canonical row ordering.

crates/orna-core/src/value.rs
    Validated ORNA-UI/1 and registered opaque-value boundaries. If a semantic
    UI identity walker is needed, it must live at this existing value boundary
    and must not expose full UI values to the projection.

crates/orna-postgres/src/kernel/inspect.rs
    Protected capture transaction, exact loaded-state handoff, event-derived
    metadata, client-offer mapping, epoch payload order, and recovery.

crates/orna-postgres/src/kernel/security.rs
    Existing sealed-dispatch capture callsites and trusted presentation,
    active-revision, observer, and invocation facts. No caller-provided
    authority may be added.

crates/orna-server/src/invoke.rs
    Existing Inspector carrier assembly, common provenance/classification
    prefix, row encoders, and canonical carrier sort. Only row population and
    its checked inputs may change.

crates/orna-server/src/inspect.rs
    Existing headless projection rendering and classifier gates; preserve its
    public JSON/error semantics.

crates/orna-client/src/inspect_session.rs
crates/orna-client/src/inspect_lifecycle.rs
    Existing client epoch binding, freeze/resume, cancellation, generation,
    and stale-completion checks; no relaxed validation.

crates/orna-core/tests/inspect_contract.rs
crates/orna-server/tests/inspect_carrier_contract.rs
crates/orna-server/tests/inspect_live.rs
crates/orna-server/tests/inspect_recursion_live.rs
crates/orna-server/tests/inspect_stale_session_live.rs
    Focused row, byte, redaction, epoch, recursion, cancellation, and installed
    headless proofs.
```

No database migration, new standard-library type, Qt source, model adapter,
gateway, Studio constructor, or canonical-spec edit is required by this
bounded decision. The implementation must keep the existing inspection
relations and registered codecs.

## Ordered implementation and proof checklist

Later agents must execute this order without widening the contract:

1. **Freeze the identity fixture.** Assert the existing function IDs, TypeIds,
   representation contracts, `ORNA-INSPECT/1` fields, projection tags, the
   nine `std.inspect.render@1` parameters, and the `INEP` persisted collection
   order. Include a decode test for an old empty projection set.
2. **Add a pure population input seam.** Make the epoch builder accept only
   the captured event batch, trusted presentation result, canonical client
   offer, active revision facts, and exact loaded state cells. Prove that the
   helper performs no database, filesystem, network, runtime, or toolkit read.
3. **Populate resource categories.** Emit the bounded State/Catalog/
   Standard/Runtime Active rows using the rules above. Prove cardinality,
   absence behavior, and that Invalidated/Released are never inferred from a
   later live read.
4. **Populate semantic UI identities.** Validate and walk captured
   `ORNA-UI/1` values within existing node/value bounds. Emit only checked
   function, call-site, and contract identities; omit absent optional call
   sites and reject malformed present identities. Prove properties, slots,
   actions, keys, source origins, and instance strings never enter the row.
5. **Capture the final presenter decision.** Thread the already-resolved
   presenter, selected offered sink, and trusted selected runtime into the
   same capture boundary. Emit the one accepted row only when the existing
   successful presentation path has an epoch; preserve no-epoch behavior for
   unresolved/failed presentation.
6. **Preserve the client-offer mapping.** Continue mapping every canonical
   runtime offer into `RuntimeBindingRow`, including descriptor and contract
   order, trust, and negative-rank clamping. Prove no path, ABI, platform,
   limits, principal, or grant leaks into the row.
7. **Commit one immutable epoch.** Construct every vector before the existing
   epoch insert and encode it in the existing `INEP` order. Prove that a later
   state write, revision activation, offer change, presenter change, or
   resource completion cannot change an old epoch.
8. **Exercise carrier encoding.** Reuse the current row encoders,
   classification markers, common provenance prefix, ORV5 wrapper, strict
   encoded-row sort, and bounds. Add exact-byte goldens for populated rows and
   malformed/duplicate/unsorted/trailing/overflow cases; all failures must be
   atomic and stable.
9. **Exercise authorization and redaction.** Prove scope denial, independent
   Values/Source/SecurityDetails/RuntimeInternals grants, structural row
   visibility, typed redaction markers, and the absence of sensitive bytes in
   rows or errors. Do not add richer per-field policy.
10. **Exercise epoch lifecycle and recursion.** Prove stale, future, foreign,
    target-mismatched, principal-mismatched, and revision-mismatched carriers;
    exact freeze/resume tokens; cancellation and shutdown; stale resource and
    Inspector completions; observer suppression; and root/descendant
    `inspect.recursion` rejection.
11. **Run the installed headless proof.** Start an ordinary CLIENT Inspector,
    capture a target with the available semantic facts, materialize all nine
    renderer arguments, and verify deterministic `std.ui.UI` output through
    `std.inspect.render@1`. Prove an absent headless provider fails closed as
    `inspect.runtime_unavailable` and does not select a graphical runtime.
12. **Audit the deferred boundary.** Review the diff and proof artifacts for
    accidental resource request IDs, stream rows, full UI values, native
    handles, model contracts, trace streaming, Qt/Studio constructor behavior,
    gateway exposure, or caller authority. Any such addition is outside this
    ADR and must be removed or separately decided.

## Proof obligations

The decision is ready for implementation acceptance only when the focused
proofs demonstrate all of the following:

1. New populated rows and old empty rows round-trip through the same canonical
   `ORNA-INSPECT/1` and `INEP` codecs with identical identity and field order.
2. Every row is sourced from captured immutable facts, has no unbounded value,
   and cannot be changed by a later mutable read.
3. Resource rows remain category/status-only and bounded; no resource request,
   stream, generation, argument, or result identity is present.
4. UI rows contain only checked function/call-site/runtime-contract identity;
   full UI property, slot, action, key, source, instance, and toolkit data is
   absent.
5. Presentation rows match the existing final resolver decision and selected
   offer facts, with no fabricated candidate on a failed/no-epoch path.
6. Runtime rows are the canonical client-offer projection and expose no local
   library path or authority.
7. All existing scope, classifier, redaction, observer, recursion,
   freeze/resume, cancellation, stale-completion, and stable-error guarantees
   remain true.
8. Overflow, malformed data, unknown or wrong carriers, stale/foreign epochs,
   and inconsistent capture inputs fail closed before partial publication.
9. The ordinary headless Inspector remains the only exercised rendering path;
   this slice proves no Qt, model, gateway, or Studio constructor semantics.

## Precedence and sources

This ADR narrows only the previously empty implementation portion of the
existing work boundary. The source-of-truth order remains canonical spec,
then accepted work ADRs, then implementation.

* `spec/docs/30-inspector.md`, `spec/docs/31-self-inspection.md`, and
  `spec/api/inspect.md` define the Inspector areas, immutable epochs, observer
  context, privileges, and redaction.
* Work ADR 0064 remains authoritative for server-side snapshots, projection
  identities, trace, and the privilege ladder.
* Work ADRs 0080 and 0081 remain authoritative for transient `@1` carriers,
  headless ordinary CLIENT execution, provenance, redaction, recursion,
  freeze/resume, and `std.inspect.render@1`.
* Work ADRs 0070, 0071, 0077, 0078, and 0079 remain authoritative for USER
  state and resource identity/lifecycle/transport/action behavior; this ADR
  observes none of their richer request or stream semantics.
* Work ADRs 0062, 0063, 0076, and 0082 through 0085 remain authoritative for
  `std.ui.UI`, local runtime offers, the headless boundary, and the separate
  production Qt/package boundary. None is broadened here.
* `crates/orna-core/src/inspect.rs`, `inspect_carrier.rs`, `value.rs`,
  `crates/orna-postgres/src/kernel/inspect.rs`, and
  `crates/orna-server/src/invoke.rs` are the implementation evidence for the
  field order, bounds, identities, and capture sources named above.
