# ADR 0080: Headless Ordinary CLIENT Inspector v1

**Status:** Accepted

**Implementation status:** The headless ordinary CLIENT Inspector v1 and its
focused installed proof are implemented. Work ADR 0081 supersedes only the
product-specific `devtools.*` naming with the generic `std.inspect.render@1`
contract; this ADR remains historical authority for the headless carrier,
provenance, redaction, recursion, and no-toolkit constraints.

## Decision

Accept the smallest executable Inspector surface as an ordinary, user-declared
CLIENT function:

```sql
CREATE CLIENT FUNCTION devtools.inspector(
    p_target REF sys.inspect.invocation
)
RETURNS std.ui.UI;
```

The function is ordinary application source. It is not a sealed system
function, a privileged runtime callback, or a second invocation gateway. It
may inspect only the supplied target and immutable projection carriers for
that target. Returning `std.ui.UI` does not execute the target UI, invoke a
native widget, or grant database access.

The first runtime proof is deliberately headless. It evaluates the ordinary
CLIENT function, constructs a deterministic semantic UI value, and validates
canonical `ORNA-UI/1` bytes through the test-only runtime fixture accepted by
work ADR 0076. This ADR does not accept a graphical runtime, production runtime
ABI, shared-library loader, toolkit binding, or reflective gateway.

The stable helper seam is:

```sql
CREATE EXTERNAL CLIENT FUNCTION devtools.inspector_shell(
    p_snapshot                sys.inspect.snapshot,
    p_invocation_nodes        sys.inspect.invocation_nodes,
    p_calls                   sys.inspect.calls,
    p_resources               sys.inspect.resources,
    p_state_cells              sys.inspect.state_cells,
    p_ui_nodes                sys.inspect.ui_nodes,
    p_presentation_candidates  sys.inspect.presentation_candidates,
    p_runtime_bindings        sys.inspect.runtime_bindings,
    p_security_decisions      sys.inspect.security_decisions
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'devtools.inspector_shell@1';
```

`devtools.inspector_shell@1` is a pure semantic projection helper. It accepts
only the typed carriers below, creates no server work, reads no process or
filesystem state, and emits no native toolkit operation. Its result is an
immutable transient UI value. A headless runtime may render or compare that
value; it may not interpret it as permission to load a graphical runtime.

The exact body spelling was an implementation seam: the parser, checked CLIENT
plan, and evaluator had to agree on the ordinary expression/state form before
source was accepted. The signature and helper contract were stable for this
slice. Work ADR 0081 now governs the generic render contract and rejects an
unregistered helper, wrong carrier version, or carrier from a different
snapshot.

## Sealed snapshot and projection carriers

The inspect values are sealed transient standard-library values. They are not
durable database objects and cannot be persisted, placed in USER state, or
passed across principals or active revision pairs.

The required type identities and codec contracts are:

```sql
CREATE TYPE sys.inspect.snapshot AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.snapshot@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.invocation_nodes AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.invocation_nodes@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.calls AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.calls@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.resources AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.resources@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.state_cells AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.state_cells@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.ui_nodes AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.ui_nodes@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.presentation_candidates AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.presentation_candidates@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.runtime_bindings AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.runtime_bindings@1'
    IMMUTABLE TRANSIENT;
CREATE TYPE sys.inspect.security_decisions AS VALUE
    OPAQUE KERNEL CONTRACT 'orna.sys.inspect.security_decisions@1'
    IMMUTABLE TRANSIENT;
```

These declarations require registered codecs; they do not make arbitrary
application opaque values inspectable. The sealed operations are materialized
and bounded, not streams:

```sql
sys.inspect.snapshot(
    p_target  REF sys.inspect.invocation,
    p_options sys.inspect.snapshot_options
) RETURNS sys.inspect.snapshot;

sys.inspect.invocation_nodes(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.invocation_nodes;
sys.inspect.calls(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.calls;
sys.inspect.resources(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.resources;
sys.inspect.state_cells(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.state_cells;
sys.inspect.ui_nodes(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.ui_nodes;
sys.inspect.presentation_candidates(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.presentation_candidates;
sys.inspect.runtime_bindings(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.runtime_bindings;
sys.inspect.security_decisions(p_snapshot sys.inspect.snapshot)
    RETURNS sys.inspect.security_decisions;
```

These eight projections use the already-reserved sealed function identities
`0x03` through `0x0b` in order: snapshot, invocation nodes, calls, resources,
state cells, UI nodes, presentation candidates, runtime bindings, and security
decisions. Trace identity `0x0c` is deliberately not part of v1.

The result-carrier TypeIds use the next reserved bytes without colliding with
existing carriers: `sys.inspect.invocation_nodes` is `...f8`, `calls` is
`...f9`, `resources` is `...fa`, `state_cells` is `...fb`, `ui_nodes` is
`...fc`, `presentation_candidates` is `...fd`, `runtime_bindings` is `...fe`,
and `security_decisions` is `...ff`. Existing `...f3` through `...f6` remain
`invocation`, `snapshot`, `snapshot_options`, and `trace_event`; `...f7`
remains `sys.security.principal`. These are stable TypeIds, not an invitation
for application code to allocate values in the sealed range.

If an implementation retains the public API spelling `REF
sys.inspect.snapshot`, that is a sealed, non-persistable handle with these same
provenance and lifetime rules, not an application object reference. A stream
return type, durable snapshot object, or generic JSON projection requires a
new contract version and ADR.

### Carrier framing

Each carrier uses its registered canonical opaque-value contract and contains a
bounded materialized row vector. Payload values use canonical ORV5 frames and
the existing bounded active-value codec. It is neither JSON nor an
implementation-private serialization.

The canonical envelope is ordered as follows:

```text
magic                 "ORNA-INSPECT/1 "
carrier_version       u16 = 1
projection_tag        u8
epoch_id              u64
source_revision_id    canonical source revision identity
catalogue_revision_id canonical catalogue revision identity
row_count             bounded u32
rows                  canonical length-delimited row frames
```

The existing canonical identity widths, byte order, and active-value bounds
apply. Encoders use one field order and big-endian integers. Decoders reject
unknown versions/tags, truncation, duplicate or unsorted identity fields,
invalid nested values, excessive row counts, and trailing bytes. Carrier
construction is atomic. Epoch and revision evidence are kernel-generated, not
caller-supplied authority.

### Projection row schemas

The following schemas are fixed for `@1`. Classified fields contain a canonical
typed value only when authorized; otherwise they contain a typed redaction
marker. Structural identity remains visible when only a value classifier is
denied.

Common fields:

```text
epoch_id, row_ordinal, owner_scope,
source_revision_id, catalogue_revision_id,
function_id / function_revision (when applicable),
call_site_id (when applicable), classification_evidence
```

`invocation_nodes@1`:

```text
invocation_id, parent_invocation_id?,
target_function_id / target_revision, call_site_id?,
owner_scope, phase, outcome, started_sequence,
completed_sequence?, source_span_or_digest (classified)
```

`calls@1`:

```text
invocation_id, call_ordinal,
callee_function_id / callee_revision, call_site_id,
target_domain, result_type_id, outcome,
argument_digest, arguments (classified)
```

`resources@1` contains the complete resource cache identity and lifecycle:

```text
resource_request_id,
target_function_id / target_revision, principal_scope,
arguments_digest, invalidation_token,
resource_generation, resource_kind,
status (idle | loading | ready | failed | cancelled),
request_sequence?, completion_sequence?, failure_code (public only)
```

`state_cells@1`:

```text
function_instance_id, state_slot_id,
state_scope (local | session | user), declared_type_id,
value_presence, value (classified), state_revision
```

`ui_nodes@1` records semantic identity rather than toolkit handles:

```text
function_instance_id, call_site_id, semantic_node_key,
runtime_contract_name / runtime_contract_version,
parent_node_key?, ordinal, mount_revision,
properties / slots / actions (classified)
```

`presentation_candidates@1`:

```text
candidate_id, semantic_node_key?, sink_type_id,
runtime_contract_name / runtime_contract_version,
capability_names, selection_status
```

`runtime_bindings@1`:

```text
binding_id, runtime_name / runtime_version,
contract_names / contract_versions, selected_sink_type_id,
feature_names, platform_capabilities (classified), binding_revision
```

`security_decisions@1`:

```text
decision_id, invocation_id, requested_scope,
requested_classifier, decision (allowed | denied | redacted),
reason_code, security_revision
```

Rows cannot contain credentials, raw secrets, unbounded source text,
caller-supplied authority, or opaque values without registered codecs. The
resource identity is the complete `ClientResourceKey`: target identity and
revision, principal scope, canonical argument digest, and invalidation token.

## Snapshot, epoch, and lifetime

`sys.inspect.snapshot` captures one immutable inspection epoch from the target
invocation and already-recorded server inspection facts. Projections read only
that epoch; they never query live mutable state after capture. The snapshot
binds:

```text
server_epoch_id, target_invocation_id,
captured source/catalogue RevisionPair,
authenticated principal scope,
observer invocation context, capture policy, classifier set
```

The CLIENT execution has a distinct `client_epoch_id`; it is not the server
epoch. Every carrier is bound as:

```text
client_epoch_id -> server_epoch_id -> captured RevisionPair
```

The client rejects a carrier whose revisions differ from its active
`RevisionPair`. The server rejects a request whose target, principal, observer
context, or requested revision differs from its captured epoch. A new source or
database revision creates a new snapshot and cannot mutate an old one.

### Freeze and resume

The helper may freeze its snapshot/UI model and return a transient token bound
to:

```text
client_epoch_id, server_epoch_id, target_invocation_id,
principal identity, captured RevisionPair, projection versions
```

Resume is valid only in the same live client execution and for the exact token.
A stale, foreign-principal, future-epoch, or revision-mismatched token returns
a stable error without partial UI. There is no implicit refresh while frozen;
refresh captures a new snapshot and token. Tokens are never persisted in USER
state or sent through a SERVER resource.

Snapshot and carrier lifetime ends when the owning CLIENT invocation is
cancelled, the client runtime shuts down, the principal changes, or the active
revision is replaced. Cancellation invalidates pending operations and handles;
a late result cannot publish into a later epoch.

## Observer context and recursion suppression

The Inspector root carries both the observed target invocation and the Inspector
observer invocation. The client supplies observer root and parent identities
from its execution context; the server never accepts caller-supplied identity
as authority. Every request propagates:

```text
observer_root_invocation_id, observer_parent_invocation_id,
observed_target_invocation_id, observer_purpose = inspect
```

By default, captured data excludes the observer invocation, its helper
invocations, and effects caused solely by observing. Suppression uses invocation
lineage and purpose, not display names. If the target is the Inspector root or
one of its descendants, the request fails with `inspect.recursion` rather than
executing or rendering the target recursively.

`include_observer_effects` is not in the ordinary v1 source surface. A later
privileged diagnostic surface may add it with a bounded event budget and a new
contract version. v1 therefore has no feedback loop in which rendering changes
the inspected snapshot. The Inspector displays semantic UI-node projections;
it never executes an inspected `std.ui.UI` value.

## Privilege and redaction

Inspection scope and value classification are independent decisions:

| Scope | Meaning |
| --- | --- |
| `OwnInvocation` | Authenticated principal owns the target invocation. |
| `SessionInvocations` | Target belongs to the authenticated session. |
| `AnyInvocation` | Explicit protected diagnostic authority for another principal. |

| Classifier | Examples |
| --- | --- |
| `Values` | State, arguments, resource values, UI properties. |
| `Source` | Source spans, source text, source digests. |
| `SecurityDetails` | Detailed policy inputs and security evidence. |
| `RuntimeInternals` | Runtime platform and binding capabilities. |

A decision must satisfy both requested scope and classifier. Denied scope
rejects with `inspect.denied`; a visible row whose classifier is not granted
remains structurally present with a typed redaction marker and
`classification_evidence = redacted`. Redaction is per classified field.
Errors and audit records contain stable public codes only, never raw values,
credentials, policy inputs, or arguments.

The ordinary Inspector requests `OwnInvocation` and structural data by default.
Values, source, security details, and runtime internals require their
corresponding protected grant. Source and carriers cannot contain a principal,
grant, or `run_as` authority.

## Identity, versions, and errors

The checked plan and carrier header pin the function and function revision,
`devtools.inspector_shell@1`, each `orna.sys.inspect.*@1` codec, target
function and `RevisionPair`, call-site/invocation identities, client/server
epochs, and projection kind/version.

Stable error codes include:

| Code | Meaning |
| --- | --- |
| `inspect.invalid_target` | Target is not a valid inspect invocation reference. |
| `inspect.unknown_carrier` | Carrier type or codec is not registered. |
| `inspect.malformed_carrier` | Framing, identity, row, or value is invalid. |
| `inspect.limit` | Snapshot, row count, or payload exceeds a bound. |
| `inspect.denied` | Scope authorization failed. |
| `inspect.epoch_mismatch` | Target, principal, observer, or revision differs. |
| `inspect.stale_epoch` | Snapshot or freeze token is no longer current. |
| `inspect.future_epoch` | Snapshot/token belongs to a future epoch. |
| `inspect.recursion` | Observer would inspect itself or descendants. |
| `inspect.cancelled` | Owning invocation or operation was cancelled. |
| `inspect.closed` | Client/runtime lifetime has ended. |
| `inspect.runtime_unavailable` | Required headless contract is not installed. |
| `inspect.projection_failed` | Protected capture failed without exposing details. |

Errors are terminal for the affected operation. They cannot overwrite a later
generation; retries allocate a new request identity and repeat all provenance
and authorization checks.

## Focused proof and installed proof

The accepted implementation proof covers:

1. ordinary parsing and checked-plan acceptance of the exact signature, rejecting
   an unregistered helper or wrong return type;
2. codec registration, ORV5/active-value framing, canonical field order, bounds,
   malformed input, and trailing-byte rejection;
3. all eight materialized projections, including empty rows, identity, revision
   evidence, and resource/UI fields;
4. immutable snapshot capture despite later source/catalogue/state changes;
5. client/server epoch binding, freeze/resume, stale/future token rejection,
   principal mismatch, and revision mismatch;
6. observer lineage, default self-suppression, recursion rejection, and no
   feedback loop;
7. independent scope/classifier authorization, structural redaction, stable
   errors, and no sensitive bytes in errors;
8. cancellation, shutdown, stale completion, bounded rows, and no late publish
   into another client epoch;
9. deterministic `devtools.inspector_shell@1` output as canonical ORNA-UI/1,
   with no DB, filesystem, network, toolkit, or native-runtime access;
10. an installed end-to-end proof that starts an ordinary CLIENT Inspector,
    captures a target, materializes all eight carriers, returns `std.ui.UI`, and
    reaches the test-only headless fixture's terminal state.

The installed proof must also show that an absent headless contract fails closed
with `inspect.runtime_unavailable`; it must not select a graphical runtime.

## Exact implementation artifacts

```text
crates/orna-syntax/src/       CLIENT signature/state/callback grammar
crates/orna-compiler/src/     checked Inspector plan and carrier calls
crates/orna-client/src/       evaluation, epoch binding, helper seam, codecs,
                              cancellation, and headless proof
crates/orna-core/src/         carrier identities, row schemas, epoch/error model
crates/orna-server/src/       sealed dispatch, authorization, capture,
                              observer propagation, and redaction
crates/orna-postgres/src/     immutable epoch capture from protected invocations
crates/orna-standard/src/     verified type and codec registrations
crates/orna-system-tests/     installed ordinary CLIENT and negative proofs
docs/decisions/0080-client-inspector.md
```

The implementation artifacts and focused proof for this boundary are complete.
Later refinements must retain the same headless, immutable, redacted, and
no-toolkit constraints; work ADR 0081 records the generic render-contract
refinement.

## Alternatives considered

### Make `sys.inspect.*` the Inspector function

Rejected. The Inspector is ordinary application CLIENT source. Sealed system
functions own protected capture and projection only; making one the UI function
would couple application UI to privileged server execution.

### Return generic JSON or an untyped row object

Rejected. Generic JSON loses nominal type, revision, identity, redaction, and
codec boundaries and invites unverified bytes to become authority. v1 uses one
registered immutable carrier per projection.

### Return a live stream

Rejected for v1. The current sealed signatures do not define stream item types,
cursors, backpressure, or cancellation for inspect projections. Materialized
bounded carriers make snapshot and lifetime rules testable. Trace streaming is
a separate contract.

### Implement a graphical runtime first

Rejected. Work ADR 0076 accepts only the in-process headless fixture. A
graphical runtime would silently promote unresolved ABI, loader, event-loop,
ownership, and platform decisions.

### Let the runtime inspect the target UI directly

Rejected. The runtime receives immutable semantic projections and never executes
the target UI, preventing feedback loops and preserving the security seam.

## Deferred and proposal-only surface

The following remain outside accepted Inspector v1:

- graphical widgets, native toolkit bindings, accessibility bridges, browser
  deployment, and production event-loop integration;
- production shared-library loading, runtime discovery, platform selection,
  trust policy, and runtime offers;
- the production C ABI in `spec/spec/orna_runtime_abi_v1.h`;
- live trace streams, cursors, backpressure, and incremental projection updates;
- arbitrary observer effects or unbounded `include_observer_effects`;
- durable snapshot objects, cross-principal carrier reuse, and persisted tokens;
- generic JSON, arbitrary opaque codecs, reflective gateways, and caller authority;
- source editing, revision application, hot reload, or execution of inspected
  targets from the Inspector;
- resource scheduling policy beyond work ADRs 0071, 0077, and 0078.

## Precedence

The canonical requirements in `spec/docs/30-inspector.md`,
`spec/docs/31-self-inspection.md`, and `spec/api/inspect.md` remain
authoritative. Work ADR 0064 remains authoritative for the server-side
`sys.inspect` epoch, projections, trace model, and privilege ladder. Work ADRs
0062 and 0076 remain authoritative for transient `std.ui.UI` and the test-only
ORNA-UI/1 boundary. Work ADR 0068 remains authoritative for CLIENT expressions
and runtime contracts; ADR 0069 for CLIENT state declarations; ADRs 0071, 0077,
and 0078 for resource identity, language, and transport. Work ADR 0081
supersedes the product-specific render naming while retaining this ADR's
headless carrier, epoch, observer, and helper constraints. The sealed
`sys.invoke` boundary remains authoritative for SERVER execution and security.
