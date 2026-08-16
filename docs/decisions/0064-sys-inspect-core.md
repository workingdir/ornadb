# ADR 0064: The Inspector Core — sys.inspect Snapshot, Projections, and Trace

**Status:** Accepted

## Decision

The server-side Inspector core becomes real: immutable `sys.inspect` snapshot
epochs captured during protected invocations, eight closed projections over an
epoch, a sequence-addressable trace stream, an INSPECT privilege ladder, and an
`orna inspect` CLI surface. This slice implements the spec `api/inspect.md`
contract without any client UI: the `devtools.inspector` CLIENT function and
its `std.ui` widgets are blocked on the runtime-contract syntax and are
explicitly deferred.

## Background

The sealed `sys.invoke` route (ADR 0053/0054/0055) produces an in-memory
`InvocationEventBatch` (Started / ValueBatch / Completed) and appends
decision-only audit rows. Nothing retains a usable inspection record: typed
arguments and results are never persisted, trace policy is carried but never
consumed, and `InvocationEventBody` has only the six sealed event families.
The spec `api/inspect.md` defines the inspection surface; the current repo has
no `sys.inspect.*` registration (the sealed registry holds exactly
`sys.catalog.health` and `sys.invoke`, `crates/orna-core/src/system.rs`).

## Surface

```text
sys.inspect.snapshot(
    p_invocation REF sys.inspect.invocation,
    p_options    sys.inspect.snapshot_options
) RETURNS REF sys.inspect.snapshot;

sys.inspect.invocation_nodes(p_snapshot)
sys.inspect.calls(p_snapshot)
sys.inspect.resources(p_snapshot)
sys.inspect.state_cells(p_snapshot)
sys.inspect.ui_nodes(p_snapshot)
sys.inspect.presentation_candidates(p_snapshot)
sys.inspect.runtime_bindings(p_snapshot)
sys.inspect.security_decisions(p_snapshot)

sys.inspect.trace(
    p_invocation REF sys.inspect.invocation,
    p_after_sequence BIGINT DEFAULT 0
) RETURNS STREAM<sys.inspect.trace_event>;
```

`orna inspect <invocation> [--projection <name>] [--trace] [--after <n>]`
renders JSON lines, mirroring the `orna state get|set` render path.

## Immutable snapshot epochs

A snapshot is an immutable inspection epoch, modeled after the existing
`VerifiedStandardLibrarySnapshot` pattern (verified, Arc-immutable, pinned by
revision pair) rather than `sys.state` (which has no epoch concept). Every
capture is a new `_orna_kernel.inspect_snapshots` row keyed by a new
`InspectEpochId`, holding the pinned source/catalogue revision pair, the owner
principal, the invocation id, and a canonical ORV5 `summary_bytes` payload.
Rendering effects therefore appear only in later epochs, matching
`docs/31-self-inspection.md`.

## Privilege ladder

New closed `InspectPrivilege` set (spec `api/inspect.md`): OwnInvocation,
SessionInvocations, AnyInvocation, Values, Source, SecurityDetails,
RuntimeInternals. Scaling is a ladder (OWN then SESSION then ANY) with the
value/source/security/runtime classifiers orthogonal. Denials fail closed
with an `inspect:%` reason and an `Inspect` audit kind.

## Projections over one epoch

Each projection is a read-only query over the epoch plus live protected
relations, gated by the ladder and classification redaction:

| Projection | v1 content | Source of facts |
| --- | --- | --- |
| invocation_nodes | root invocation node only | epoch (no nested calls in sealed v1) |
| calls | root call + ValueBatch summary | epoch + captured batch schema |
| resources | empty | no resource tracking exists yet |
| state_cells | root-function state cells, values redacted unless INSPECT VALUES | `user_state_cells` |
| ui_nodes | empty | CLIENT execution blocked |
| presentation_candidates | accepted presenter + sink + selected runtime | dispatch path capture |
| runtime_bindings | offered runtime(s), selected family | client offer (ADR 0063) |
| security_decisions | linked execute/capability/user_state decisions | `security_audit_events` |

## Trace stream

`sys.inspect.trace_event` rows carry `(invocation_id, sequence, kind, payload,
recorded_at, observer_invocation, purpose)`. `p_after_sequence` filters
`sequence > $after ORDER BY sequence`. Default suppression drops rows whose
`observer_invocation` is the inspecting invocation (self-observation, spec
`docs/31`); the observer context is threaded from the request carrier, which
is already encoded/decoded but never consumed today.

## Sealed registration

Ten new sealed `sys.inspect.*` entries (`snapshot`, `trace`, eight
projections) with fixed `FunctionId`s after `...02`, new
`SystemFunctionKind`/signature types, sealed carrier identities for
`sys.inspect.invocation` / `sys.inspect.snapshot` /
`sys.inspect.snapshot_options` / `sys.inspect.trace_event`, and
representation contracts `orna.sys.inspect.*@1`. The sealed-registry length
test (`system.rs`) is updated.

## Deferred (documented, not invented)

- `devtools.inspector` and all `std.ui` widgets: require `RUNTIME CONTRACT`
  syntax and CLIENT expression bodies (blocked, orna-syntax ownership).
- `ui_nodes` and rich `presentation_candidates` (per-candidate scores): need
  planner instrumentation and the CLIENT VM.
- Resource records: need `std.data.Resource` tracking (separate slice).
- `sys.state.*` as a callable sealed function: only the CLI path exists
  today; `state_cells` reads `user_state_cells` directly in this slice.

## Consequences

- New `crates/orna-core/src/inspect.rs` (pure model), `system.rs` registry
  extension, `security.rs` privilege decision, migration 0027
  (`inspect_snapshots`, `inspect_trace_events`, audit extension),
  `crates/orna-postgres/src/kernel/inspect.rs`, `crates/orna-server/src/inspect.rs`
  + `Command::Inspect`, and end-to-end proof tests mirroring
  `user_state_live.rs`.
- Capture is wired into `dispatch_sealed_sys_invoke`: after the protected
  decision and before/at execution, persist trace rows and one snapshot from
  the produced event batch plus decision facts.
- No owned-path changes; no new dependencies; `Cargo.lock` untouched.