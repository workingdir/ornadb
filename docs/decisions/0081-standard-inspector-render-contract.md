# ADR 0081: Generic Standard Inspector Render Contract

**Status:** Accepted

## Context

Work ADR 0080 accepted a headless Inspector as an ordinary `CLIENT` function, but
made the product-specific names `devtools.inspector` and
`devtools.inspector_shell@1` part of the helper seam. That naming couples a
reusable inspection facility to one application name.

The canonical design already treats `sys.inspect.*` capture and projections as
standard functionality. It also treats UI composition as ordinary `CLIENT`
functions that return `std.ui.UI`, with typed runtime contracts. The current
core contract is `INSPECT_RENDER_CONTRACT = "std.inspect.render@1"` and its
ordered nine-carrier signature is the stable implementation boundary.

## Decision

### Keep inspection in the reusable core

`sys.inspect.*` capture, immutable snapshot and projection carriers,
provenance, redaction, and sealed operation semantics remain standard core
functionality. Carriers remain immutable, transient, bounded, and bound to the
capture epoch, revision evidence, observer context, and authorization facts.
The inspection plane does not become application-owned code, and it does not
introduce a graphical runtime or toolkit.

### Make the Inspector an ordinary application function

The Inspector is a normal user-declared `CLIENT` function returning
`std.ui.UI`. Its qualified function name belongs to the application and is
arbitrary; no `devtools.*` name is reserved or required. The function uses the
standard `sys.inspect.*` APIs and may call the generic render contract below.

### Identify rendering by contract, not by function name

The runtime helper is an external `CLIENT` function declared with the standard
contract `std.inspect.render@1`. Its application-qualified function name is
not part of contract identity. The contract accepts exactly these nine ordered
carriers and returns `std.ui.UI`:

```text
p_snapshot
p_invocation_nodes
p_calls
p_resources
p_state_cells
p_ui_nodes
p_presentation_candidates
p_runtime_bindings
p_security_decisions
```

The compiler validates the registered contract and this exact signature: nine
parameters, this order, the corresponding sealed `sys.inspect.*` carrier types,
and the `std.ui.UI` return type. It does not recognize a helper because its
function name is `devtools.inspector_shell` or any other particular name.

Server and client adapters are generic providers keyed by the contract
identity and version. A provider consumes only the typed inspection carriers
and produces the immutable semantic UI value. It does not execute the
inspected UI, create server work, read process or filesystem state, or emit a
native toolkit operation.


### Migration of historical installed revisions

`devtools.inspector_shell@1` remains a historical contract spelling. This slice
does not add an alias, rewrite old source, or add a back-compat dispatch path.
An already-installed revision that uses that spelling is decoded only when the
existing artifact decoder explicitly preserves historical decoding. If it does
not, the revision fails closed as an unknown or unavailable external contract;
it is not silently reinterpreted as `std.inspect.render@1`.

New or recompiled source must declare `std.inspect.render@1`. An application
that needs the corrected behavior publishes a revision using the new contract
and may choose any application-owned Inspector function name. Keeping the
migration explicit avoids accepting a product-specific provider under a
standard identity without a decoder and preserves deterministic failure for
unknown contracts.

## Alternatives considered

### Keep `devtools.*` as the stable identity

Rejected. The `sys.inspect.*` plane and its carriers are reusable standard
functionality, so a product-specific helper identity would prevent ordinary
applications from providing their own Inspector function without adopting
Devtools naming.

### Alias `devtools.inspector_shell@1` to `std.inspect.render@1`

Rejected for this slice. An alias would silently change the meaning of
historical installed artifacts and would require an explicit compatibility
policy, decoder support, and migration proof. Historical decoding remains an
implementation choice; it is not added by this ADR.

### Add a graphical runtime or toolkit

Rejected. The accepted boundary remains headless and semantic. Graphical
runtime loading, toolkit bindings, and native event-loop behavior are separate
future contracts.

## Consequences

- Any ordinary `CLIENT` application can provide an Inspector without using a
  reserved product namespace.
- Compiler, server, and client code share one contract identity and one ordered
  carrier signature.
- Provider lookup and failure behavior are deterministic and fail closed.
- Existing `devtools.inspector_shell@1` revisions require preserved historical
  decoding or an explicit source migration; this ADR does not provide a
  compatibility alias.
- No graphical runtime, toolkit, or native ABI is accepted by this decision.

## Precedence and sources

This ADR supersedes the product-specific naming decision in work ADR 0080:
`devtools.inspector` and `devtools.inspector_shell@1` are no longer canonical.
ADR 0080 remains historical context for the headless ordinary `CLIENT`
Inspector and its immutable carrier, provenance, redaction, recursion, and
no-toolkit constraints.

The relevant source-of-truth documents are:

- `spec/docs/30-inspector.md` for the ordinary Inspector function and public
  inspection plane;
- `spec/docs/31-self-inspection.md` for immutable epochs and observer context;
- `spec/api/inspect.md` for inspection projections, privileges, and redaction;
- `spec/docs/20-ui-dsl.md` and `spec/api/ui-runtime.md` for ordinary `CLIENT`
  UI values and typed runtime contracts;
- `spec/docs/25-source-compiler-ir.md` and `spec/api/compiler.md` for stable
  semantic identities and contract checking;
- `docs/decisions/0064-sys-inspect-core.md` for the reusable `sys.inspect` core;
- `crates/orna-core/src/inspect.rs` for `INSPECT_RENDER_CONTRACT` and the
  ordered nine-carrier signature.
