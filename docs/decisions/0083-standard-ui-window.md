# Work ADR 0083: Register the v1 `std.ui.window` Client Function

**Status:** Accepted

## Canonical decision

Spec ADR 0019 registers the first accepted UI entry point in a new append-only
`orna.std/7` snapshot. This work ADR records the implementation boundary. The
canonical ADR is authoritative for identities, declaration text, and function
semantics.

## Implementation

The work keeps all V1-V6 standard snapshots unchanged and appends
`std/window.orna` as V7 source unit ordinal 6. The unit contains the exact
external CLIENT declaration for `std.ui.window(title TEXT, content std.ui.UI)`
and runtime contract `std.ui.window@1`.

The compiler validates the declaration, prepares its versioned external client
plan, and includes it in standard CLIENT target resolution. The local CLIENT
evaluator resolves standard executables from the pinned standard snapshot before
it dispatches the contract through `ClientResourceExecutor::external_contract`.
The runtime adapter remains host-owned. A missing adapter fails closed; the
standard function does not load native code or select a path.

The standard snapshot pipeline remains append-only. V1-V6 digests, origins,
source units, function identities, and executable records are not rewritten.

## Deferred

This decision does not define a general UI JSON-to-ABI transport, list/table
model completion, launch metadata, database-backed Studio operations, or a
second runtime. Those contracts need separate canonical decisions.
