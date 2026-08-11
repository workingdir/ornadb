---
title: UI and runtimes
description: std.ui.UI as a standard-library value type, installed native runtimes, automatic selection, and per-user state.
---

# UI and runtimes

`std.ui.UI` is a standard-library value type. CLIENT functions return it. The local `orna` client selects an installed runtime to materialise it.

:::warning Development status
The `std.ui.UI` concept and the runtime model are LOCKED. The runtime ABI and exact UI syntax are CURRENT PROPOSAL. OrnaDB is under active development; there is no released executable yet.
:::

## What std.ui.UI is

`std.ui.UI` is a standard-library transient value type. It is not:

- a core language keyword;
- a persistent OrnaDB object;
- a schema definition kind;
- a Qt, GTK, or DOM widget;
- an untyped JSON object;
- a mandatory result type for every program.

A CLIENT function can return a value of that type:

```sql
CREATE CLIENT FUNCTION studio.main()
RETURNS std.ui.UI
AS
    std.ui.window(
        title   => 'Orna Studio',
        content => studio.workspace()
    );
```

The function definition is durable. The returned value is transient and immutable. UI functions do not mutate widget objects. They return a fresh declarative value, and the client reconciles it with the mounted graph.

## Why not a core UI keyword

The core language should not know that graphical interfaces exist. The generic architecture is:

```text
function returns TypeId X
client advertises a consumer for TypeId X
sys.invoke selects a presentation path to X
local runtime consumes X
```

`std.ui.UI` is one standard-library example. The same architecture can support `std.terminal.Document`, `std.service.Service`, and other surfaces.

## Runtime families

A runtime is a locally installed shared library. Database source cannot silently install native code.

```text
orna-runtime-tty       terminal, streams, documents
orna-runtime-qt        Windows, macOS, Linux
orna-runtime-gtk       Linux
orna-runtime-swiftui   macOS
orna-runtime-imgui     Windows, macOS, Linux, browser possibility
orna-runtime-web       browser environment
```

## Automatic selection

The ordinary command does not name a runtime:

```bash
orna invoke studio.main
```

The client evaluates:

```text
target and result requirements
configured user preferences
current environment (TTY, desktop, browser)
installed runtimes
runtime contract versions
security and trust policy
```

An override is advanced and local:

```bash
orna --runtime qt invoke studio.main
```

The server must not force the client to load a particular native shared library. The server plans to typed sinks and contracts. The client chooses the implementation.

## Runtime contracts

A graphical runtime registers implementations of external CLIENT functions:

```text
std.ui.window@1
std.ui.dockspace@1
std.ui.tree@2
std.ui.code_editor@2
std.ui.data_grid@2
```

Every runtime implementation must pass the same contract suite: property defaults, slot cardinality, event payload types, state round trips, focus behaviour, accessibility labels, and hot reload.

## The runtime boundary

A runtime receives typed values and contract operations from `orna`. It does not authenticate, execute SQL, write USER state, or call the database directly.

```text
runtime <-> orna client <-> OrnaDB server
```

This keeps Qt, GTK, SwiftUI, ImGui, and Web implementations focused on local materialisation. A runtime crash terminates or detaches the local invocation. It does not damage the database server.

## State scopes

State declarations live inside CLIENT functions:

```sql
CREATE CLIENT FUNCTION studio.workspace_shell()
RETURNS std.ui.UI
IS
    STATE layout std.ui.DockLayout
        SCOPE USER
        DEFAULT studio.default_layout();
BEGIN
    RETURN std.ui.dockspace(
        layout           => layout,
        on_layout_change => layout.SET
    );
END;
```

| Scope | Lifetime | Example |
|---|---|---|
| `LOCAL` | mounted function instance | hover state, current drag |
| `SESSION` | root invocation or client session | selected node, unsaved buffer |
| `USER` | durable in OrnaDB, keyed by principal | dock layout, preferred output view |

`USER` state is keyed server-side by the authenticated principal. The client never sends a principal ID in a state write. The server derives it from the session.

Each declared state slot has a stable `StateSlotId`. A semantic rename keeps the slot identity, so the persisted value survives.

## Resources and actions

CLIENT UI functions must never block the local runtime event loop while waiting for a remote SERVER function.

A resource is a reactive handle to a typed asynchronous computation, usually a SERVER call:

```sql
LET rows std.data.Resource<TABLE(...)> :=
    std.data.resource(tasks.overdue);
```

An action is a typed value triggered by a runtime event:

```sql
std.ui.button(
    label    => 'Save',
    on_click => std.action.call(
        studio.save_document,
        document,
        buffer
    )
)
```

Actions may update state, call a CLIENT function, call a SERVER function asynchronously, or open a semantic function invocation.

## Next steps

- Read the CLIENT domain in [functions](/docs/functions/).
- Read how results are planned in [invocation](/docs/invocation/).
- Read the process topology in [architecture](/docs/architecture/).

Return to the [OrnaDB frontpage](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
