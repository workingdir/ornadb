---
title: OrnaDB documentation
description: Start with the object-relational program model, then follow a function from source to invocation and output.
---

# OrnaDB documentation

OrnaDB is an object-relational database that stores and runs the programs which work with its data. Types describe values and durable objects. Functions describe all executable behaviour. Invocations create running programs.

:::warning Development status
OrnaDB is under active development. The documentation distinguishes locked design decisions, current proposals, open questions, and implemented work. Examples show the intended language unless a page says that a feature is implemented.
:::

## The language centre

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

A CLIENT function can return `std.ui.UI`. The local `orna` process then selects a compatible installed runtime. The database server does not select or deliver a native shared library.

## Read the system from its source

- [Getting started](/docs/getting-started/) follows one type and function from source to output.
- [Object model](/docs/object-model/) covers object identity, value types, and typed `REF<T>` references.
- [Functions](/docs/functions/) defines the SERVER and CLIENT execution domains.
- [Invocation](/docs/invocation/) explains `sys.invoke`, presenters, sinks, and automatic runtime planning.
- [UI and runtimes](/docs/ui-and-runtimes/) defines `std.ui.UI`, state, resources, and the local runtime boundary.
- [Architecture](/docs/architecture/) shows the process topology, compiler, bootstrap rings, and trust boundaries.
- [Security and inspection](/docs/security-and-inspection/) covers principals, capabilities, audit, and the Inspector.
- [Examples](/docs/examples/) traces the connected source set.
- [Status](/docs/status/) separates implemented work from design and open questions.
- [Glossary](/docs/glossary/) defines the terms used across the guide.

Return to the [OrnaDB frontpage](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
