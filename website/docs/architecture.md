---
title: Architecture
description: The process topology, bootstrap rings, trust boundaries, compiler pipeline, and the self-hosting ring.
---

# Architecture

OrnaDB is one database that stores data, object types, function definitions, source, revisions, grants, program state, and the tools used to inspect those things.

:::warning Development status
The process boundaries and bootstrap ring model are LOCKED. The accepted local Inspector boundary and the Qt v1 runtime profile are bounded implementation slices. The runtime ABI and compiler internals remain CURRENT PROPOSAL. Full Studio, the security/DBA console, and reflective JSON-RPC/MCP gateways are CURRENT PROPOSAL (CONCEPTUAL), not released features. OrnaDB is under active development; there is no released executable yet.
:::

## Process topology

```text
Client machine
    orna
        connection and session
        CLIENT function VM
        presenter and sink negotiation
        automatic runtime manager
            orna-runtime-tty
            orna-runtime-qt
                accepted v1: Linux x86_64, Qt 6 Widgets, ABI v1.0
            other runtime families
                CURRENT PROPOSAL or FUTURE
            |
            | typed protocol over TCP/TLS, Unix socket, or named pipe
            v
OrnaDB server
    authentication and sessions
    object-relational storage and SQL
    transactions and stable IDs
    source, definitions, and revisions
    SERVER function execution
    sys.invoke planning
    durable USER state
```

Local and remote databases use the same semantic model. Convenience may discover or start a local daemon:
The examples use `studio.main` as a CURRENT PROPOSAL (CONCEPTUAL) full-Studio target; they do not claim a released Studio. The accepted narrower dogfooding boundary is the ordinary CLIENT Inspector.


```bash
orna --db ./workspace invoke studio.main
```

A remote database:

```bash
orna --db orna://db.example.com/work invoke studio.main
```

## Bootstrap rings

OrnaDB is self-hosting, but it cannot depend on a graphical tool in order to repair the graphical tool.

```text
Ring 0   native trusted kernel
        storage, WAL, SQL, stable IDs, authentication, enforcement,
        typed value codec, raw call primitive

Ring 1   mandatory system functions
        sys.invoke, sys.compiler, sys.revisions, sys.security,
        sys.inspect

Ring 2   standard library
        std.ui, std.terminal, std.json, std.csv, std.xml,
        std.data, std.present, std.launch, std.service, std.protocol

Ring 3   dogfooded/client tools
        headless Inspector v1 (accepted bounded boundary)
        Orna Studio and security/DBA console
            CURRENT PROPOSAL (CONCEPTUAL)
        state inspector, launcher, test runner
            CURRENT PROPOSAL (CONCEPTUAL)
```
Ring 3 is a model boundary, not a release inventory. The accepted slice is the headless Inspector v1. Full Studio and security/DBA-console dogfooding remain conceptual/current proposals and are not released.

Ring 3 tools are ordinary OrnaDB programs. Native code is reserved for trust, performance, and bootstrap boundaries, not convenience.

## Trust boundaries

```text
server kernel                 highest trust
system catalog and std source trusted, versioned, inspectable
SERVER functions              sandboxed and authorised by the server
CLIENT functions              sandboxed in the local client VM
orna client                   trusted local executable
installed native runtime      explicitly installed native trust
remote database               may publish untrusted CLIENT artifacts
```

Connecting to an unknown database must never grant its CLIENT functions arbitrary filesystem, process, credential, or network access.

Two invariants follow from this model:

- The runtime does not speak the database protocol.
- The server cannot choose native code. It plans to typed sinks and contracts. The local client selects the installed runtime.

## Compiler pipeline

```text
source bytes
    -> lossless CST
    -> AST
    -> resolved semantic graph with stable IDs
    -> type, domain, security and capability checks
    -> semantic IR
    -> SERVER and CLIENT artifacts
    -> semantic diff
    -> transactional apply
```

Source is human-facing. Developers edit `.orna` files using Git, editors, CI, and code review. Definitions have IDs independent of display names:

```text
TypeId, FieldId, FunctionId, ParameterId,
StateSlotId, CallSiteId, RuntimeContractId
```

A semantic rename keeps the ID. Deleting and adding a definition creates a new ID.

## Revisions and hot reload

Function revisions are immutable. A running invocation pins a revision. An edit creates a candidate revision, never mutates active executable memory.

```text
FunctionId(studio.main)
    current -> RevisionId(42)
    history -> 39, 40, 41, 42
```

Apply is atomic:

```text
parse source
resolve names
check types, domains, security, capabilities
compute semantic diff
compile artifacts
validate dependencies and migrations
commit source plus semantic definitions
publish an invalidation event
```

Any failure rolls back the whole apply. A hot reload can patch a compatible instance, remount a function subtree, restart the root invocation, or reject the new revision.

## Dogfooding

The accepted Inspector is an ordinary CLIENT function returning `std.ui.UI`. It uses public introspection APIs and bounded snapshot epochs to inspect another invocation and itself. Orna Studio and the security console are database-resident shapes under CURRENT PROPOSAL (CONCEPTUAL); their full dogfooding is not released. The narrower local `std.security` administration-function boundary remains distinct from that unresolved console.

The Inspector and runtime boundaries therefore test the model without implying that the full Studio, security console, or reflective gateway surfaces already exist.

## Next steps

- Read the executable model in [functions](/functions/).
- Read the invocation flow in [invocation](/invocation/).
- Read the trust and inspection model in [security and inspection](/security-and-inspection/).

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
