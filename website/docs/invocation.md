---
title: Invocation
description: How orna invoke reaches sys.invoke, how results are presented, and how --output and --explain work.
---

# Invocation

Every external program begins with one operation: invoke a function stored in OrnaDB under the current authenticated session and deliver its result to this client.

:::warning Development status
The `sys.invoke` concept and raw typed call boundary are LOCKED. Request and event details and broader presenter/runtime extensions are CURRENT PROPOSAL. The accepted production graphical profile is Qt v1 on Linux x86_64; full Studio and reflective JSON-RPC/MCP gateways are CURRENT PROPOSAL (CONCEPTUAL), not released features. OrnaDB is under active development; there is no released executable yet.
:::

## The universal entrypoint

```bash
orna invoke <qualified-function-name> [arguments]
```

The CLI does not special-case every function, output encoding, UI runtime, or protocol. It constructs a typed request and uses a raw bootstrap call to the mandatory system function `sys.invoke`.
A `studio.main` target below is only a CURRENT PROPOSAL (CONCEPTUAL) full-Studio example; it does not claim a released Studio.


```text
orna client
    |
    | CALL_RAW(@sys.function/invoke, request)
    v
OrnaDB server
    |
    v
sys.invoke(request)
```

`CALL_RAW` only understands typed frames, the authentication context, streaming, and cancellation. It contains no JSON, UI, TTY, or presenter policy.

Logical signature:

```sql
CREATE SYSTEM SERVER FUNCTION sys.invoke (
    p_request sys.invoke.Request
)
RETURNS STREAM<sys.invoke.Event>;
```

## CLI forms

```bash
orna invoke tasks.overdue
orna invoke tasks.overdue --before 2026-08-01
orna invoke tasks.overdue --output json
orna invoke tasks.overdue --output csv > overdue.csv
orna invoke tasks.overdue --explain
orna invoke studio.main  # conceptual full Studio target (CURRENT PROPOSAL)
```

`--output` sets an explicit output requirement. `--output json` asks `sys.invoke` to find a JSON presentation path. The target function is unchanged.

Parameter flags are sugar for typed arguments. The parameter `p_before` maps to `--before`. The canonical request binds a stable `ParameterId` to a typed value.

## Planning phases

`sys.invoke` runs these phases for a root call:

1. Resolve the target name or ID and pin a function revision.
2. Bind supplied arguments to stable parameter IDs and evaluate defaults.
3. Authorise the session principal and check `EXECUTE` privilege.
4. Execute the target. SERVER targets run on the server. CLIENT targets run in
   the local `orna` process through the accepted evaluator and Stage 1
   control-plane boundary; the full production VM sandbox remains a proposal.
5. Obtain the canonical typed result. No presentation has occurred yet.
6. Plan presentation from the result type to a client-offered sink.
7. Run presenter functions.
8. Negotiate runtime contracts for the selected sink.
9. Stream typed events back to the client.

## Four terms

```text
Canonical result    the typed value the target function returns

Presenter           a registered function that transforms one result type
                    into another type or surface

Sink                a type or surface the local client can consume

Runtime             a local installed implementation that consumes
                    one or more sink types
```

Examples:

```text
TABLE
    -> std.terminal.present_table
    -> std.terminal.Document
    -> orna-runtime-tty

TABLE
    -> std.json.encode
    -> std.io.ByteStream(application/json)
    -> stdout
```

Presenter selection is a typed graph search, not a single switch statement. Ranking considers the explicit output requirement, caller context, client sink preferences, type specificity, streaming compatibility, runtime contract support, and cost.

## Automatic behaviour

| Caller | Target result | Explicit output | Likely plan |
|---|---|---|---|
| interactive TTY | table | none | terminal presenter to TTY runtime |
| interactive TTY | table | `json` | JSON encoder to stdout |
| pipe or CI | table | `json` | JSON encoder to stdout |
| desktop launcher | table | none | conceptual UI-table plan; accepted graphical v1 is the Qt runtime profile |
| CLI | `std.ui.UI` | none | direct UI sink to an automatically selected installed runtime |
| CLI | `std.ui.UI` | `json` | reject unless an explicit debug presenter exists |
| JSON-RPC/MCP gateway (CONCEPTUAL) | any supported | protocol JSON | CURRENT PROPOSAL reflective protocol presenter; no released gateway |
| CLIENT function | typed value | none | direct typed value, no root presentation |

### Reflective gateways (conceptual)

A JSON-RPC or MCP gateway is modeled as a reflective CLIENT program with explicit exposure metadata. This is CURRENT PROPOSAL (CONCEPTUAL), not universal automatic exposure or a released transport. Gateway identity comes from configured service or delegated authentication; request data cannot select an OrnaDB principal.


## Explainability

```bash
orna invoke tasks.overdue --explain
```

displays the same plan the accepted bounded Inspector can expose through its projections:

```text
TARGET
    tasks.overdue@17 (SERVER)

RESULT
    TABLE(title TEXT, due_at TIMESTAMP)

CALLER
    CLI_TTY

CANDIDATES
    std.json.encode
        compatible, not selected: no JSON requirement

    std.ui.present_table
        compatible, not selected: GUI sink lower preference

    std.terminal.present_table
        compatible, selected: TTY-preferred sink

FINAL SINK
    orna-runtime-tty / stdout
```

Automatic selection without an explain plan is unacceptable.

## Event stream and channels

The response is a typed event stream, not one blob. Core events include invocation start, target resolution, security decisions, revision pinning, presenter candidates, runtime offers, value batches, and completion or failure.

Channel discipline keeps machine output pure:

```text
result bytes and documents  -> stdout
human diagnostics           -> stderr
interactive progress        -> terminal control channel
inspection trace            -> Inspector or trace sink
```

```bash
orna invoke tasks.overdue --output json > tasks.json
```

must never corrupt JSON with warnings or progress text.

## Cancellation and recovery

The client receives an `InvocationId` early and may cancel. Cancellation propagates to the target, presenters, streams, and client VM tasks. Transaction cancellation follows server transaction semantics.

A raw recovery path bypasses presenter and runtime planning:

```bash
orna raw-call sys.catalog.health
```

It requests canonical typed events and works even if `sys.invoke` or the standard presenters are broken.

## Next steps

- Read the SERVER and CLIENT domains in [functions](/functions/).
- Read how `std.ui.UI` reaches a runtime in [UI and runtimes](/ui-and-runtimes/).
- Read the trust model in [security and inspection](/security-and-inspection/).

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
