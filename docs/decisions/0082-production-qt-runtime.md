# ADR 0082: The First Production Non-TTY Runtime Is Qt

**Status:** Accepted for the OrnaDB v1 development boundary

## Context

OrnaDB already has a TTY renderer and a test-only headless runtime fixture. It
has no production non-TTY runtime. The canonical runtime ABI and `std.ui`
pages remain proposal-level because they do not settle value ownership,
threading, callbacks, shutdown, loader trust, or toolkit selection.

The v1 product needs one usable graphical runtime before Studio can become an
ordinary CLIENT application. The first runtime must be small enough to verify
without adding a second toolkit, a new wire protocol, or a database-owned
loader. The workstation and the first release target provide Qt 6 Widgets.

This decision narrows the production proposal. It does not accept a general
browser runtime, a second native toolkit, a production CLIENT VM, a gateway,
or a populated Inspector projection.

## Decision

The first production non-TTY runtime family is `orna-runtime-qt`. It uses Qt 6
Widgets and implements the existing C-shaped runtime ABI v1 query symbol:

```text
orna_runtime_query_v1
```

The ABI version is exactly major `1`, minor `0`. The runtime is selected by the
local client from its installed runtime offers. A database plan cannot select a
library path, load code, provide a principal, or provide a grant.

The v1 provider is initially supported on Linux x86_64. The provider may expose
only capabilities that it implements and tests. A second platform or toolkit
requires a later decision and must pass the same semantic contract suite.

## ABI and value boundary

The public ABI remains the type and function table in the canonical
`orna_runtime_abi_v1.h` header. The implementation must not add fields to the
v1 table or change the meaning of an existing field.

The `orna` client owns all typed values, CLIENT function instances, resource
requests, actions, state, invocation tracing, and database communication. The
runtime receives validated UI operations and immutable value references. It
never authenticates, speaks the OrnaDB protocol, executes SQL, chooses a
principal, or writes USER state.

`OrnaValueRefV1::canonical_encoding` is borrowed for the duration of the ABI
call. The runtime may inspect it during that call. If it retains a value for a
widget, callback, or model request, it must copy the bytes into runtime-owned
storage. A runtime-owned `OrnaOwnedBytes` is released only through its supplied
release function. Null pointers are valid only when the associated length is
zero.

The accepted `std.ui.UI` value remains the `ORNA-UI/1` length-prefixed UTF-8
frame. The JSON body follows the closed UI value schema. The runtime rejects a
wrong magic, truncated length, invalid UTF-8, malformed closed value, trailing
bytes, or a body above the existing bounded payload limit before it creates a
surface or widget.

## Runtime offer

The Qt descriptor reports:

```text
runtime_name: orna-runtime-qt
abi_major: 1
abi_minor: 0
thread_model: ORNA_THREAD_MODEL_CALLER_PUMPS
sink: std.ui.UI
contract: std.ui.window@1
```

The descriptor reports the actual runtime version, build identity, platform,
feature bits, and limits. It must not advertise a sink, contract, feature, or
limit that the loaded binary does not implement.

The first contract set is closed to these semantic names:

```text
std.ui.window@1
std.ui.text@1
std.ui.button@1
std.ui.panel@1
std.ui.row@1
std.ui.column@1
std.ui.text_input@1
std.ui.tabs@1
```

The implementation may reject a contract that is not present in the descriptor.
It must return `ORNA_STATUS_UNSUPPORTED` for an operation or feature outside
this set. The runtime does not infer a widget contract from an arbitrary string.

## Thread and re-entry model

The client owns the runtime thread. `create`, `create_surface`,
`destroy_surface`, `apply_ui_batch`, `set_surface_visible`,
`capture_semantic_state`, `capture_opaque_state`, `apply_model_rows`,
`cancel_request`, `request_shutdown`, and `destroy` must run on that thread.

The runtime uses `ORNA_THREAD_MODEL_CALLER_PUMPS`. `poll_event_loop` processes Qt
events and emits client callbacks in a deterministic order. `start_event_loop`
returns `ORNA_STATUS_UNSUPPORTED`; it must not start a hidden runtime thread.

A client callback must not synchronously call a mutating runtime entry point
before the current callback returns. Such re-entry returns
`ORNA_STATUS_BUSY`. Release callbacks remain valid during value cleanup and do
not count as runtime mutation.

## Handles and ownership

Runtime and surface handles are non-zero, created by the runtime, scoped to
their owning runtime, and never reused during that runtime's lifetime. The
current ABI has no node/action allocator, so node and action fields in mount
and bind operations are non-zero client reservation aliases. The runtime
adopts an alias only once, rejects aliases already live in another surface,
and retires each accepted alias permanently when its node or action ends.
Model and request handles emitted by the runtime follow the runtime-created
rule.

The runtime rejects a zero, foreign, retired, or wrong-kind handle with
`ORNA_STATUS_INVALID_ARGUMENT` or `ORNA_STATUS_NOT_FOUND` according to whether
the handle can be classified. A failed operation does not transfer ownership
or partially register a handle.

A surface owns its nodes, action bindings, models, and requests. Destroying a
surface retires all of its owned handles, cancels its pending model requests,
and emits one terminal surface-closed event. Destroying the runtime first
requires shutdown and then destroys every remaining surface.

## UI operations and atomic revisions

The v1 runtime accepts the existing operation kinds for mount, unmount,
property set/clear, child insert/remove/move, and action bind/unbind. Focus and
accessibility operations return `ORNA_STATUS_UNSUPPORTED` in v1 because the
current ABI table has no accepted payload for those operations.

The following properties have closed v1 meaning:

| Contract | Properties |
| --- | --- |
| `std.ui.window@1` | `title` |
| `std.ui.text@1` | `text` |
| `std.ui.button@1` | `label`, `enabled` |
| `std.ui.text_input@1` | `text`, `placeholder`, `enabled` |
| all container contracts | no required properties; children use declared slots |

A property value must use the declared canonical type. Unsupported property
names, wrong value types, invalid slot names, duplicate action bindings, and
out-of-range child ordinals fail before the batch changes the surface.

A batch has one `semantic_revision`. The revision must be strictly greater than
the surface revision. The runtime validates every operation, handle, contract,
property, value, slot, and ordinal against a copy of the current surface. It
commits the copy only when every operation succeeds. A stale or invalid batch
leaves the previous surface unchanged and returns
`ORNA_STATUS_STALE_REVISION` or `ORNA_STATUS_INVALID_ARGUMENT`.

A successful batch is the only point at which the surface revision advances.
Semantic capture returns the committed revision and a deterministic closed
representation. Capture never exposes raw database arguments, credentials,
USER state values, or unbounded opaque bytes.

## Events, models, and cancellation

The runtime sends only the typed event variants defined by the v1 header. It
serializes events in the caller-pumps loop. Every action event includes the
owning surface, node, action handle, and a validated payload reference. It does
not include a principal or grant.

The Qt v1 provider emits action, surface-closed, and diagnostic events. It does
not advertise list/table model contracts in this provider version. Model range,
child, completion, and cancellation semantics remain a later provider
extension because the current ABI has no accepted model-construction operation.

For every model request that a later provider creates, the client completes or
fails the request exactly once and the runtime rejects late, foreign, duplicate,
or malformed completion data. `cancel_request` remains idempotent for that
future request contract. Surface destruction, runtime shutdown, and client
disconnect cancel pending requests exactly once.

## Shutdown and failure reporting

A new runtime starts operational. `request_shutdown` changes it to draining:
new surfaces, batches, model requests, and callbacks are rejected. The runtime
cancels pending model requests, drains already queued terminal callbacks, and
then marks itself terminal. Repeated shutdown is idempotent after the drain.

`destroy` is valid only after the runtime is terminal. It releases Qt objects,
retired handles, copied values, and callback state. No callback runs after
destroy returns.

The runtime uses only the closed ABI status codes:

```text
INVALID_ARGUMENT  malformed input, value, property, or handle
UNSUPPORTED       absent v1 operation, contract, or feature
NOT_FOUND         retired object or request
BUSY              forbidden callback re-entry
CANCELLED         one cancelled model request
FAILED            Qt surface or widget failure
INTERNAL          runtime invariant or allocation failure
STALE_REVISION    non-increasing semantic revision
```

Messages are bounded diagnostics for local logs. They must not contain raw
arguments, credentials, principal data, or opaque value bytes. The client maps
these statuses to its existing redacted runtime errors.

## Loader and trust boundary

The local client loads only a runtime selected from its installed, authenticated
runtime package. The loader verifies the ABI major/minor and the descriptor
before it calls `create`. It rejects a missing query symbol, a null descriptor,
wrong ABI, malformed descriptor, missing `std.ui.UI` sink, missing
`std.ui.window@1`, unsupported thread model, or a runtime whose advertised
limits exceed the client limits.

The database cannot order a runtime load. A runtime cannot change the active
catalogue revision, security session, or invocation principal. Package
authentication remains the Debian distribution authority defined by work ADR
0047; this ADR does not add a runtime signature authority or a database-loaded
binary path.

## Source and generated files

The implementation of this decision must keep one public ABI source and make
the following changes in the implementation repository:

```text
../spec/spec/orna_runtime_abi_v1.h             canonical ABI input
runtimes/qt/CMakeLists.txt                      Qt runtime build
runtimes/qt/src/runtime.cpp                    Qt runtime implementation
runtimes/qt/tests/runtime_test.cpp             Qt runtime contract tests
crates/orna-client/src/runtime_loader.rs       trusted loader and descriptor gate
crates/orna-client/src/lib.rs                   runtime provider wiring
crates/orna-server/src/invoke.rs                runtime offer and selection wiring
justfile                                         build and test entry points
packaging/debian/orna.install                    installed runtime payload
```

The Qt runtime may use private headers under `runtimes/qt/include`, but it must
include the canonical ABI header rather than copy it. Generated build output
must remain outside the source tree and must not become a second ABI source.

## Proof obligations

Before this boundary is promoted to the product acceptance baseline, the
repository must contain:

1. a C syntax and layout check against the canonical header;
2. a descriptor test for exact ABI, sink, contract, thread, feature, and limit
   values;
3. handle ownership tests across two surfaces and after destruction;
4. atomic batch tests for stale revisions, invalid properties, foreign handles,
   and rollback after a late operation failure;
5. callback tests for action order, model completion, cancellation, forbidden
   re-entry, and shutdown drain;
6. a Rust loader test for missing symbols, incompatible descriptors, and the
   accepted Qt descriptor; and
7. an installed Qt smoke path that creates a window, applies a closed UI value,
   processes one action, and shuts down without a callback after destroy.

The current headless fixture remains the conformance oracle for semantic
behaviour. It is not relabelled as the Qt runtime and does not prove Qt display
integration.

## Alternatives considered

### Keep only the headless fixture

This avoids native dependencies but does not provide a production UI or
unblock Studio. It remains the correct fallback for tests, not the v1 runtime.

### Select a browser runtime

A browser runtime would fit a web Studio but its deployment, process boundary,
security, and browser event-loop contract are open in the canonical spec. It is
not safe to implement from the current proposal.

### Select ImGui or GTK

Both are viable later providers. Selecting one now would add another toolkit
choice without improving the first contract proof. A second runtime is a
separate compatibility test after Qt is stable.

## Consequences

The first graphical provider has a real toolkit and a bounded, testable ABI.
Qt becomes a native build dependency for the provider, while the core client
remains independent of Qt and can continue to run with TTY or headless paths.
Linux x86_64 is the first supported non-TTY platform. The client must fail
closed when the provider is absent or incompatible rather than silently using a
different runtime.

The standard library still needs an accepted `std.ui.window@1` declaration and
stable identity allocation before source can call the contract. The populated
Inspector, production CLIENT VM, launch metadata, Studio, and gateways remain
separate contracts and are not accepted by this ADR.
