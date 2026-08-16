# ADR 0061: Durable USER State Service

**Status:** Accepted

## Decision

OrnaDB gains the durable per-principal USER state service described by spec
ADR 0007 and spec/api/state.md. `sys.state.*` becomes a protected
server-side subsystem with two closed operations:

```text
sys.state.load_user_state(
    p_root_function  REF sys.function,
    p_state_profile  TEXT,
    p_instances      SET OF sys.state.instance_request
) RETURNS SET OF sys.state.cell;

sys.state.write_user_state(
    p_changes SET OF sys.state.change
) RETURNS SET OF sys.state.write_result;
```

The server derives the principal from the authenticated session. A normal
client never sends a principal identity; the request cannot choose another
principal. Every write carries the expected revision and fails closed on a
conflict (spec ORNA0902).

## Storage model

One durable relation `_orna_kernel.user_state_cells` with the logical key:

```text
principal_id          (from the authenticated session, never the request)
root_function_id      (stable FunctionId)
root_state_profile    (TEXT, empty means the default profile)
function_id           (the state-slot's owning function)
function_instance_key (TEXT; the function-instance identity)
state_slot_id         (stable StateSlotId)
```

and the value columns: the typed value encoding (canonical ORV5 of the
runtime value), the value `TypeId`, a monotonic revision number, and the
`updated_at` timestamp. The relation is private to `_orna_kernel` with no
public grants.

The key is the full logical tuple: changing any component creates a
distinct cell. A semantic rename preserves `StateSlotId`, so the user keeps
the persisted value; delete-and-recreate a slot creates a new identity and
a new cell.

## Load

`load_user_state` returns every cell matching the authenticated principal,
the root function, the state profile, and the requested function
instances (a caller supplies instance keys; the empty set requests the
default instance key for each requested slot). Values decode through the
canonical typed codec against the recovered active revision and its opaque
registry. A cell whose type no longer matches the slot's declared type
fails closed with ORNA0901 (type incompatible) rather than silently
returning a stale value.

## Write

`write_user_state` accepts a set of changes. Each change carries:

```text
root_function_id, state_profile, function_id, instance_key, state_slot_id,
expected_revision, typed value
```

and no principal identity. The server validates every change against the
authenticated session's principal and the active revision's state-slot
facts (slot exists, owns the function, declared type matches). A change with
the expected revision equal to the current revision writes the new value and
increments the revision; any other expected revision fails closed with
ORNA0902 and returns the current revision so the client can reconcile. All
changes commit atomically or not at all.

## Audit and redaction

USER state operations are auditable: the protected audit records the
operation kind, principal, root function, and cell counts — never the typed
value payloads. Sensitive state values (a future classification) are
redacted from inspection. An administrator inspecting another principal's
state requires an explicit privilege; ordinary clients can only read/write
their own cells.

## Required implementation order

1. `docs(state): define the durable USER state service` — this ADR and the
   work-ADR index only.
2. `feat(core): model USER state cells` — the closed cell/change/write-result
   model with the logical key, revision arithmetic, and validation facts in
   orna-core, with tests.
3. `feat(postgres): register USER state storage` — an append-only migration
   for `_orna_kernel.user_state_cells` (PK on the logical key, revision
   monotonicity, typed-value columns) and its bootstrap registration.
4. `feat(postgres): load and write USER state` — the two protected kernel
   operations with session-principal derivation, type validation, atomic
   write with revision conflict handling, and audit appends.
5. `feat(server): expose the USER state service` — the `sys.state.*`
   protected surface (sealed function registry entries or the raw
   dispatch path; decide by reading how `sys.catalog.health` is exposed)
   and the closed CLI/`sys.invoke` caller.
6. `test(server): prove USER state end to end` — a live proof that a
   principal writes, loads, conflicts (ORNA0902), and reloads its own
   cells; another principal's cells are unreachable; a type change fails
   closed with ORNA0901; reopen preserves the cells.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

State-slot declarations in CLIENT source (`STATE ... SCOPE USER`) and the
CLIENT-side debounce/coalescing lifecycle are later CLIENT VM ADRs.
Administrative cross-principal inspection, state classification/redaction
rules, merge functions for structured state, and the full `state_profile`
semantics beyond the default profile are later decisions. `LOCAL` and
`SESSION` scopes remain client-side in-memory state, outside this service.

## Precedence

This decision implements spec ADR 0007 and spec/api/state.md. Work ADR 0020
remains authoritative for authenticated sessions; the state service derives
its principal from that session. Work ADR 0060 remains authoritative for the
CLIENT capability gate. The canonical specification remains authoritative
outside this accepted implementation scope.
