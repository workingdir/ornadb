---
title: OrnaDB
description: "Object-relational native applications: programs, data, and tools in one database."
---

# OrnaDB

OrnaDB is an object-relational database that stores and runs the programs which work with its data. Types describe values and durable objects. Functions describe all executable behaviour. Invocations create running programs.

:::warning Development status
OrnaDB is under active development. This guide distinguishes locked design decisions, current proposals, open questions, and implemented work. Examples show the intended language unless a page says that a feature is implemented.
:::

## The model

Every executable definition is a function. The locked model has two execution domains:

- `CREATE SERVER FUNCTION` runs beside the data under database authority.
- `CREATE CLIENT FUNCTION` runs in the sandboxed local `orna` process.

A running program is a rooted graph of function invocations. Durable objects retain identity; typed `REF<T>` values connect them without reducing them to untyped keys.

```sql
CREATE TYPE crm.customer AS OBJECT (
    name  TEXT NOT NULL,
    email TEXT UNIQUE
);

CREATE SERVER FUNCTION crm.find_customers(p_search TEXT)
RETURNS SET OF REF crm.customer
AS
    SELECT REF(c)
      FROM crm.customer c
     WHERE c.name ILIKE '%' || p_search || '%';
```

```bash
orna invoke crm.find_customers --search acme
orna invoke crm.find_customers --search acme --output json
```

The target function does not change with the output surface. In the current presentation-planning proposal, `sys.invoke` resolves and authorises the function, executes it, then finds a path from its typed result to a compatible local sink.

A CLIENT function can return `std.ui.UI`. The local `orna` process selects a compatible installed runtime; the database server does not select or deliver a native shared library.

## Read the system

- [Getting started](/getting-started/) follows one type and function from source to output.
- [Object model](/object-model/) covers object identity, value types, and typed `REF<T>` references.
- [Functions](/functions/) defines the SERVER and CLIENT execution domains.
- [Invocation](/invocation/) explains `sys.invoke`, presenters, sinks, and automatic runtime planning.
- [UI and runtimes](/ui-and-runtimes/) defines `std.ui.UI`, state, resources, and the local runtime boundary.
- [Architecture](/architecture/) shows the process topology, compiler, bootstrap rings, and trust boundaries.
- [Security and inspection](/security-and-inspection/) covers principals, capabilities, audit, and the Inspector.
- [Examples](/examples/) traces the connected source set.
- [Status](/status/) separates implemented work from design and open questions.
- [Glossary](/glossary/) defines the terms used across the guide.

Start with [Getting started](/getting-started/) or inspect the [source repository](https://github.com/workingdir/ornadb).
