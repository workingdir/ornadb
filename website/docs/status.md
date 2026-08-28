---
title: Status
description: Implemented repository work, accepted bounded slices, locked decisions, deferred proposals, open questions, and future work.
---

# Status

This page separates what exists from what is designed. It summarises the repository delivery checklist and the canonical v0.2 design status.

:::warning Development status
OrnaDB is under active development. The repository builds and verifies a local
Debian package, but no public release is available to install today.
:::

## Implemented repository work

These items are implemented, reviewed, committed, and verified locally in the
working repository:

| Item | Status |
|---|---|
| Offline `orna source check <file.orna>` | IMPLEMENTED |
| SERVER query and mutation slices through required unique reference fields | IMPLEMENTED |
| First verified Boolean CLIENT function path | IMPLEMENTED |
| Stable catalogue value-type and binding identities | IMPLEMENTED |
| Versioned standard-library and catalogue hashes without changing version-1 bytes | IMPLEMENTED |
| Source-independent standard type manifest | IMPLEMENTED |
| Orna-owned instance, initialisation, private socket, and foreground supervision | IMPLEMENTED |
| Native `orna server backend-shell` | IMPLEMENTED |
| One-executable Debian package with embedded PostgreSQL | IMPLEMENTED |
| Stage 1 CLIENT VM structural admission and in-memory host-control boundary (no production host effects) | IMPLEMENTED |

These are contract and design slices in the repository. They do not yet form a usable product.

## Accepted bounded slices

The following source and contract slices are accepted, but do not imply a public
release. Environment-gated proofs remain deferred.

- **Qt v1 runtime/provider/package — ACCEPTED (BOUNDED).** The first production non-TTY provider is `orna-runtime-qt` on Linux x86_64: Qt 6 Widgets, ABI v1.0, and caller-pumps. It is a separately installed package with the fixed `/usr/lib/orna/liborna-runtime-qt.so` path and Debian repository authentication; the local `orna` client selects an installed offer. A test-only headless fixture shares the ABI v1 semantic contract.
- **TTY/presenter/output — ACCEPTED (BOUNDED).** `orna-runtime-tty` is the accepted terminal renderer. Typed presenter planning and optional `--output` (for example, `--output json`) are accepted; TTY is a runtime, while JSON, CSV, and XML are encoded outputs. Installed evaluator proof and the production TTY ABI remain deferred.
- **Scalar and `STREAM<T>` resources — ACCEPTED (BOUNDED).** Explicit typed resource construction is executable for scalar targets and, through `std.data.stream_resource`, `STREAM<T>` targets. `AWAIT` yields typed non-empty batches and then terminal `None`; `TABLE`/`ROWS` resource transport is deferred.
- **`std.json`/UI/action — ACCEPTED (BOUNDED).** `std.json.Value` is the immutable transient JSON value; `std.ui.UI` and `std.ui.window@1` are transient UI contracts; executable actions are bounded to `std.action.call`. `std.action.sequence` and `std.action.parallel` remain deferred.
- **V8 Rows/table presentation — ACCEPTED (BOUNDED).** `std.data.Rows` V8 codecs and retained table/CSV presentation are accepted. General Rows/object-value semantics remain deferred.
- **Bounded populated Inspector slices — ACCEPTED (BOUNDED).** Headless Inspector v1 includes populated resource, UI, presentation, and runtime projections with bounded row, redaction, and epoch contracts. Installed evaluation remains environment-gated and deferred.

## Not yet implemented

| Area | Item | Status |
|---|---|---|
| Protocol | Public protocol, authorisation, and exposure slices | DEFERRED |
| Types | Enum, record, and opaque value types beyond standard primitives; general `VALUE` semantics | DEFERRED |
| CLIENT VM | Full production CLIENT VM/sandbox, concrete host capabilities, and process isolation | DEFERRED |
| Security | Protected audit path and its production/integration proof | DEFERRED |
| Gateways | Reflective JSON-RPC/MCP gateway implementation and exposure dispatch | DEFERRED |
| Launch | `std.launch` and launch/application execution | DEFERRED |
| Data | Virtual models and `TABLE`/`ROWS` resource transport | DEFERRED |
| Dogfooding | Full Studio and security/DBA UI | DEFERRED |
| Proof | Environment-gated Compose, installed-runtime, and clean-host proofs | DEFERRED |

## Locked design decisions

| Area | Decision |
|---|---|
| Product | OrnaDB, "Object-Relational Native Applications"; CLI is `orna` |
| Executables | every executable definition is a function |
| Domains | `CREATE SERVER FUNCTION` and `CREATE CLIENT FUNCTION`; one domain per function |
| Applications | running program is a rooted function invocation graph; no `CREATE APPLICATION` |
| UI type | `std.ui.UI`, a standard-library transient value type |
| UI entry | `std.ui.window(title TEXT, content std.ui.UI)` as `std.ui.window@1` |
| JSON value | immutable transient `std.json.Value` with the `orna.std.value.json@1` codec contract |
| Invocation | root calls go through inspectable `sys.invoke` |
| Runtime | local `orna` selects an installed runtime offer; the first production non-TTY provider is bounded `orna-runtime-qt` on Linux x86_64 |
| Output | optional `--output` requirement; normal invocation is automatic; TTY and encoded outputs remain distinct |
| Identity | `sys.security.session_principal()` and related functions; no `CURRENT_USER` keyword |
| State | durable `USER` state keyed by authenticated principal |
| Resources | typed `std.data.Resource<T>` and `std.data.StreamResource<T>` with explicit `AWAIT`; scalar and `STREAM<T>` forms are accepted |
| Actions | executable v1 action is `std.action.call`; sequence and parallel remain deferred |
| Inspector | an ordinary CLIENT function using public introspection APIs |
| Security | principals are first-class catalog data with kernel enforcement |
| Gateways | reflective CLIENT direction with explicit exposure metadata; implementation deferred |
| Runtimes | explicitly installed client libraries; server never selects native code |

## Current proposals

These are the strongest concrete designs for implementation experiments. They are
not released and remain outside the accepted bounded slices:

- the full production CLIENT VM, capability sandbox, and host-effect broker;
- the protected audit path and full security/DBA application;
- reflective JSON-RPC/MCP gateways and `std.launch`;
- virtual `TableModel`/`TreeModel` models and `TABLE`/`ROWS` resource transport;
- general `VALUE` and object-value semantics beyond the accepted Rows contract;
- presenter registry/ranking and runtime ABI/toolkit extensions beyond Qt v1;
- module and package distribution beyond the fixed Qt runtime package.

## Open questions

- exact `AS VALUE` DDL and `OPTION<T>` versus nullability;
- CLIENT VM bytecode versus WASM versus custom IR;
- presenter scoring and tie-breaking beyond the bounded presenter path;
- graphical runtime extensions beyond accepted Qt v1;
- Qt list/table model construction and completion semantics;
- cleanup and defer semantics;
- module registry and dependency resolution;
- physical storage layout;
- PostgreSQL wire compatibility.

## Future work

- worker domain;
- distributed execution;
- temporal whole-program snapshots;
- semantic branches and merges;
- additional runtime families;
- portable pure functions;
- audio, document, and service surface ecosystems.

## Rejected designs

| Rejected | Replacement |
|---|---|
| `CREATE APPLICATION` | root function invocation plus launch metadata |
| `CREATE COMPONENT` | CLIENT function returning `std.ui.UI` |
| `CREATE QUERY` | SERVER function with a SQL body |
| `CREATE SCREEN` or `PAGE` | UI function composition |
| core routes | semantic function invocation |
| `RUNS ON` suffixes | `CREATE SERVER FUNCTION` and `CREATE CLIENT FUNCTION` |
| `HOST FUNCTION` terminology | the CLIENT domain |
| core `UI` keyword or type | `std.ui.UI` standard-library value type |
| `CURRENT_USER` keyword | `sys.security.session_principal()` and related functions |
| required runtime selection on each invocation | automatic local selection; optional global override |
| `--format` as a peer of runtime | optional `--output` with typed presenter planning |
| TTY equated with JSON | TTY is a runtime; JSON, CSV, and XML are encoded outputs |
| server-selected native libraries | server plans contracts; local client selects the runtime |
| automatic exposure of every function | explicit protocol `Exposure` objects |
| UI as opaque JSON | typed standard-library value and runtime contracts |
| pure EAV physical storage | typed and relational physical prototype |
| PL/SQL `EXCEPTION` tail | expected values plus structured failures |

## Sources of truth

The design bundle holds the canonical handbook, the ADRs, the grammar draft, and the logical catalog. The [getting started](/getting-started/) guide is the entry point. The [source repository](https://github.com/workingdir/ornadb) is the authoritative location for the implementation.

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
