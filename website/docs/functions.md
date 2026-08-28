---
title: Functions
description: CREATE SERVER FUNCTION and CREATE CLIENT FUNCTION. Domains, cross-domain calls, transactions, security modes, and runtime contracts.
---

# Functions

Every executable definition is a function. There is no separate definition kind for queries, procedures, components, screens, pages, or applications.

:::warning Development status
The SERVER and CLIENT domain model is LOCKED. Accepted local slices include bounded Inspector, resource, and action forms; function body details are CURRENT PROPOSAL. Full Studio and reflective JSON-RPC/MCP gateway programs are CURRENT PROPOSAL (CONCEPTUAL), not released features. OrnaDB is under active development; there is no released executable yet.
:::

## The two declarations

The domain appears at the front because it changes the function's authority, available APIs, latency model, transaction semantics, and deployment artefact.

```sql
CREATE SERVER FUNCTION ...
CREATE CLIENT FUNCTION ...
```

There is no `CREATE FUNCTION ... RUNS ON ...` suffix. The domain is part of the declaration.

## SERVER function

A SERVER function runs in the OrnaDB server environment. It handles:

- SQL and durable object access;
- transaction-bound mutation;
- constraints and authoritative validation;
- security-sensitive operations;
- catalog and introspection queries.

```sql
CREATE SERVER FUNCTION crm.rename_customer (
    p_customer REF crm.customer,
    p_name     TEXT
)
RETURNS REF crm.customer
SECURITY INVOKER
TRANSACTION ATOMIC
IS
BEGIN
    UPDATE crm.customer c
        SET c.name = p_name
        WHERE REF(c) = p_customer;

    RETURN p_customer;
END;
```

## CLIENT function

A CLIENT function runs in the local `orna` client process. Accepted local
execution includes UI values, LOCAL and SESSION state, typed scalar and stream
resources, actions, and the bounded Stage 1 VM control-plane seam. The full
production sandbox and host-capability mediation remain CURRENT PROPOSAL.

It handles:

- composition of `std.ui.UI` values;
- LOCAL and SESSION state;
- calls to SERVER functions through accepted resources and `std.action.call`;
- local capability declarations checked by the accepted gate;
- reflective JSON-RPC and MCP gateway programs, as a CURRENT PROPOSAL
  (CONCEPTUAL) using explicit exposure metadata; universal automatic exposure is
  not accepted.

> **CURRENT PROPOSAL (CONCEPTUAL):** This `studio.main` example describes the unresolved full-Studio shape; it is not a released program.
```sql

CREATE CLIENT FUNCTION studio.main()
RETURNS std.ui.UI
AS
    std.ui.window(
        title   => 'Orna Studio',
        content => studio.workspace()
    );
```

"CLIENT" describes the execution side of the database protocol. It can model a desktop app, terminal process, browser process, service gateway, CI process, or long-running agent; reflective gateway behavior remains a conceptual/current-proposal surface.

## One domain per function

A normal function has one domain. This avoids ambiguous questions:

```text
Can it directly join a transaction?
Can it access a local filesystem?
Which clock and locale does it observe?
Does calling it cross the network?
Which sandbox and capability policy applies?
```

A future pure `PORTABLE FUNCTION` may be considered only for deterministic code with strict restrictions.

## Cross-domain calls

| Call | Rule |
|---|---|
| SERVER to SERVER | normal typed call inside the server engine |
| CLIENT to CLIENT | normal local typed call in the client VM |
| CLIENT to SERVER | always asynchronous from an interactive render path; use resource and action primitives |
| SERVER to CLIENT | not synchronous; a server function may return values or enqueue notifications a client observes |

A SERVER function cannot synchronously call an arbitrary connected client. This prevents hidden dependence on a particular active desktop session.

## Transaction semantics

SERVER functions may declare:

```sql
TRANSACTION ATOMIC
TRANSACTION READ ONLY
```

`TRANSACTION MANUAL` is future work and should not appear in the first implementation accidentally.

CLIENT functions never own a server transaction simply because they are running. Each server call has explicit transaction semantics.

## Security modes

```sql
SECURITY INVOKER    -- default
SECURITY DEFINER
```

`SECURITY DEFINER` is security-sensitive. It is audited and subject to restrictions on dynamic name resolution and dependencies. See [security and inspection](/security-and-inspection/).

## External CLIENT functions and runtime contracts

Runtime and local capability contracts are declared as external CLIENT functions:

```sql
CREATE EXTERNAL CLIENT FUNCTION std.ui.window (
    title   TEXT,
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.window@1';
```

The declaration is stored in the database. An installed runtime supplies the local implementation. The accepted production v1 profile is `orna-runtime-qt` on Linux x86_64 with Qt 6 Widgets, ABI v1.0, and caller-pumps; other toolkit/runtime families remain gated.

## Capabilities

CLIENT functions may declare required local capabilities:

```sql
CREATE CLIENT FUNCTION local.hash_file (
    p_file std.fs.Path
)
RETURNS BYTES
REQUIRES CAPABILITY std.fs.read(p_file)
IS
BEGIN
    RETURN std.crypto.sha256(std.fs.read_all(p_file));
END;
```

Capabilities are checked at compilation where possible and at invocation always.

## Domain checking

The compiler must reject:

- a SERVER function calling a CLIENT-only contract;
- a CLIENT function issuing raw SQL directly;
- a CLIENT function claiming server transaction semantics;
- unguarded client capability use;
- return and value types that cannot cross the boundary;
- unsafe capture of transient client values in durable objects.

## Deliberately absent

```text
CREATE APPLICATION
CREATE COMPONENT
CREATE QUERY
CREATE SCREEN
CREATE PAGE
CREATE ROUTE
CREATE FUNCTION ... RUNS ON ...
```

Each of these was rejected because a function plus ordinary metadata covers the need.

## Next steps

- Read how functions are invoked in [invocation](/invocation/).
- Read how CLIENT functions reach a UI in [UI and runtimes](/ui-and-runtimes/).
- Read the security model in [security and inspection](/security-and-inspection/).

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
