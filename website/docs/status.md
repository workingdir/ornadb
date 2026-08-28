---
title: Status
description: Implemented repository work, locked decisions, current proposals, open questions, and future work.
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

## Not yet implemented

| Area | Item |
|---|---|
| Protocol | invocation, authorisation, and public protocol slices |
| Types | enum, record, and opaque value types beyond the standard primitives |

| CLIENT VM | Production sandbox, protected audit, concrete host capabilities, and process isolation |

## Locked design decisions

| Area | Decision |
|---|---|
| Product | OrnaDB, "Object-Relational Native Applications"; CLI is `orna` |
| Executables | every executable definition is a function |
| Domains | `CREATE SERVER FUNCTION` and `CREATE CLIENT FUNCTION`; one domain per function |
| Applications | running program is a rooted function invocation graph; no `CREATE APPLICATION` |
| UI type | `std.ui.UI`, a standard-library transient value type |
| Invocation | root calls go through inspectable `sys.invoke` |
| Runtime | selected automatically by the local `orna` client |
| Output | optional `--output` requirement; normal invocation is automatic |
| Identity | `sys.security.session_principal()` and related functions; no `CURRENT_USER` keyword |
| State | durable `USER` state keyed by authenticated principal |
| Inspector | an ordinary CLIENT function using public introspection APIs |
| Errors | expected outcomes are values; no PL/SQL-style `EXCEPTION` tail |
| Source | human source plus a resolved stable-ID semantic graph |
| Security | principals are first-class catalog data with kernel enforcement |
| Gateways | JSON-RPC and MCP are reflective CLIENT programs |
| Runtimes | explicitly installed client libraries; server never selects native code |

## Current proposals

These are the strongest concrete designs for implementation experiments. They are not released:

- exact value-type DDL and nullability syntax;
- the full production CLIENT VM, capability sandbox, and host-effect broker;
- presenter registry and ranking algorithm;
- runtime ABI v1 and threading details;
- async syntax: resources, streams, and `AWAIT`;
- the security catalog schema and DBA tooling;
- module and package distribution format.

## Open questions

- exact `AS VALUE` DDL and `OPTION<T>` versus nullability;
- CLIENT VM bytecode versus WASM versus custom IR;
- presenter scoring and tie-breaking;
- the first graphical runtime;
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
