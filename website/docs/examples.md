---
title: Examples
description: Connected Orna source examples that share types and functions, with end-to-end invocation traces.
---

# Examples

The example source files in the specification bundle share types and functions. They do not pretend every program owns a separate data silo.

:::warning Development status
These examples are ILLUSTRATIVE CURRENT PROPOSAL. They show the intended language. None of them is a released feature. The full files live in the specification bundle under `examples/`, reachable from the [source repository](https://github.com/workingdir/ornadb).
:::

## The example set

| File | Covers |
|---|---|
| `01_people_tasks.orna` | object types, references, SQL path dereference |
| `02_server_functions.orna` | query and mutation SERVER functions |
| `03_client_ui.orna` | CLIENT functions returning `std.ui.UI` |
| `04_studio_shell.orna` | dock layout, commands, resources, per-user state |
| `05_security_admin.orna` | principal, role, and grant queries plus a DBA UI |
| `06_inspector.orna` | the dogfooded Inspector and self-inspection |
| `07_jsonrpc_gateway.orna` | a reflective JSON-RPC gateway with explicit exposures |
| `08_mcp_gateway.orna` | reflected MCP tool schema and `sys.invoke` dispatch |
| `09_presenters.orna` | JSON, TTY, and UI presentation functions |
| `10_launch_entries.orna` | dogfooded launcher metadata and UI |

## Trace 1. A table in the terminal

```bash
orna invoke tasks.overdue
```

1. The CLI reflects the `tasks.overdue` signature.
2. The CLI converts flags and defaults into typed values.
3. A raw call invokes `sys.invoke`.
4. The server authenticates the principal and checks `EXECUTE`.
5. The SERVER function returns a typed table.
6. The caller context is an interactive TTY.
7. The planner chooses `std.terminal.present_table`.
8. `orna-runtime-tty` renders the document.
9. The Inspector can display every step.

The same function with an explicit output requirement:

```bash
orna invoke tasks.overdue --output json
```

The JSON path selects `std.json.encode` and writes a byte stream to stdout.

## Trace 2. A CLIENT UI function

```sql
CREATE CLIENT FUNCTION tasks.overdue_window()
RETURNS std.ui.UI
AS
    std.ui.window(
        title   => 'Overdue Tasks',
        content => std.ui.data_grid(
            source => std.data.resource(tasks.overdue)
        )
    );
```

```bash
orna invoke tasks.overdue_window
```

1. `tasks.overdue_window` is a CLIENT function.
2. The server authorises the call and pins its revision.
3. The verified CLIENT artifact is sent or loaded from cache.
4. The function returns `std.ui.UI` in the local VM.
5. The client automatically chooses a compatible installed graphical runtime.
6. The runtime materialises the contracts.
7. SERVER resources execute through nested call operations.
8. `USER` state writes are associated with the authenticated principal.

## Trace 3. The Inspector inspecting itself

```text
inv:100  studio.main()
inv:101  devtools.inspector(p_target => inv:100)
inv:102  devtools.inspector(p_target => inv:101)
```

Each Inspector is another root CLIENT function invocation. The Inspector consumes immutable inspection data. It does not execute the target UI inside itself. Rendering snapshot N produces effects that appear only in snapshot N+1, so the loop cannot feed back into itself.

CLI sugar:

```bash
orna inspect inv:123
orna invoke studio.main --inspect
```

## How to read the examples

The files build on each other. Start with `01_people_tasks.orna` for the type system, then read `02_server_functions.orna` for the SERVER domain. Read the CLIENT and UI examples only after the data model is clear.

Every example file carries the same status rule as this site: the syntax is the intended language, not a claim about what is released. The [status page](/docs/status/) lists what actually exists.

## Next steps

- Read the [getting started](/docs/getting-started/) guide for the model in five minutes.
- Read [object model](/docs/object-model/) for the type system.
- Read [invocation](/docs/invocation/) for the presentation path.

Return to the [OrnaDB frontpage](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
