# ADR 0056: `orna invoke` Binds Typed Arguments Through the Sealed Route

**Status:** Accepted

## Decision

`orna invoke <function> [arguments] [options]` becomes the first ordinary
user-facing invocation command. It reflects the target signature, converts
CLI strings to canonical typed values, builds one sealed `sys.invoke.Request`,
dispatches it through the protected kernel boundary, and renders the returned
event stream with clean channels and spec exit codes.

The command runs in-process against the fixed private instance (same host
inspection and kernel access as `orna source apply` and `orna security
grant-execute`). It does not open a second network connection, and it does not
add a new wire frame. It reuses the sealed `sys.invoke` route proven by
work ADR 0055.

## Surface

```text
orna invoke <qualified-name | canonical-function-id> [options]
```

Options (this decision; later ADRs extend):

| Option | Meaning |
| --- | --- |
| `--arg p_name=value` | one typed argument by canonical parameter id or source name |
| `--name value` | friendly sugar: `p_name` parameter maps to `--name` |
| `--args-file <path>` | typed arguments from a canonical `invoke_request` JSON document |
| `--output <alias\|media-type\|type-name>` | explicit output requirement |
| `--explain` | print the resolution, binding, and event plan; do not execute |
| `--trace off\|basic\|normal\|verbose\|profile` | trace policy |
| `--no-progress` | suppress progress diagnostics |

Exit codes follow the spec CLI table:

```text
0 success
1 target failure
2 usage / argument conversion
3 connection / authentication
4 authorization / capability
5 presentation / runtime
6 cancelled / deadline
7 protocol / internal
```

## Reflection and binding

The command resolves the target by qualified name or by canonical
`FunctionId`. For a name, resolution uses the active application catalogue
first, then the pinned verified standard catalogue; a function present in
both resolves to neither (closed ambiguity, same rule as the sealed
boundary). The resolved `FunctionDefinition` supplies the parameter list.

For each declared parameter, the CLI maps friendly `--<name>` to
`ParameterId`, converts the string to the parameter's `ResolvedType`, and
builds a typed `InvocationArgument`. Supported string conversions for this
decision:

| Parameter resolved type | CLI string form |
| --- | --- |
| `INTEGER` | decimal integer |
| `BIGINT` | decimal integer |
| `FLOAT` | decimal float |
| `BOOLEAN` | `true` / `false` |
| `TEXT` | literal text |
| `BYTES` | base64 |
| `UUID` | canonical text |
| reference (`REF T`) | `@<type-name>/<object-id>` canonical form |

An unknown flag, an unknown parameter name, a duplicate parameter, a missing
required parameter, a conversion failure, or an extra positional argument is
a usage error (exit 2). No value is sent to the database on a conversion
failure.

## Sealed request

The CLI builds `InvokeRequestInput` with:

- target: `QualifiedName` or `FunctionId` as supplied;
- arguments: the bound typed values in source order;
- caller context: `CliTty` when stdout is a terminal, `CliPipe` otherwise,
  with locale and timezone from the environment;
- client offer: protocol major 5, empty sink/runtime offer lists (the first
  presenter/runtime selection is a later ADR), default limits;
- output requirement: from `--output` when present, else none;
- trace policy: from `--trace` when present, else off.

The request is checked with `InvokeRequest::new`, encoded with
`orna_protocol::encode_invoke_request` under the recovered active revision
and its opaque codec registry, and dispatched with
`PostgresKernel::dispatch_sealed_sys_invoke` under the session authenticated
from the invoking process's Unix peer UID
(`authenticate_local_peer(geteuid())`).

## Rendering

- `InvocationStarted` and `InvocationCompleted` events are diagnostics to
  stderr unless `--no-progress`.
- Every `ValueBatch` value is written to stdout in its canonical typed
  encoding, one value per record, without progress or warning interleave.
- A `Denied` result prints one redacted denial line to stderr and exits 4.
- A bind failure prints one redacted bind line to stderr and exits 1.
- `--explain` prints the plan instead of dispatching: resolved target
  identity and revision, domain, parameters, return type, and the sealed
  request facts, then exits 0. It does not execute, authorise, or audit.

## Required implementation order

1. `docs(cli): define orna invoke typed binding` — this ADR and the work-ADR
   index only.
2. `feat(core): model CLI invocation input` — the reflection/binding helper
   over `FunctionDefinition` and `ResolvedType` with string conversions,
   plus focused conversion tests. It does not touch the sealed boundary.
3. `feat(server): invoke through the sealed route` — the in-process command
   host that reflects, binds, encodes, authenticates the local peer,
   dispatches, and renders events with clean channels and exit codes.
4. `feat(cli): parse orna invoke arguments` — the command parser and usage
   text for the surface above, with exact parse tests.
5. `test(server): prove orna invoke end to end` — a live proof that invokes
   `std.invoke.echo` by name and by identity through the command path,
   asserts clean stdout/stderr channels and exit codes, and proves usage and
   conversion failures do not dispatch.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

Presenter selection and `--output` aliases (`json`, `csv`, `table`) need the
presenter registry (spec milestone 6). `--inspect` needs the Inspector
(spec milestone 9). `--db`, `--runtime`, `describe`, `revisions`, `serve`,
and `recovery` subcommands are later ADRs. Friendly-flag values for
`DATE`, `TIME`, `TIMESTAMP`, `DURATION`, and `DECIMAL` are deferred until a
later ADR accepts their canonical text forms.

## Precedence

This decision implements the CLI surface of spec milestone 5. Work ADR 0054
remains authoritative for the sealed `sys.invoke` request and event carriers,
and work ADR 0055 for the verified standard snapshot and the echo function.
The canonical specification remains authoritative outside this accepted
implementation scope.
