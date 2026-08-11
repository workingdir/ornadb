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

The full guide covers:

- object types, value types, and typed `REF<T>` references;
- SERVER and CLIENT function domains;
- `sys.invoke` and automatic presentation planning;
- terminal, JSON, and native UI result paths;
- principals, grants, capabilities, and per-user state;
- the dogfooded Inspector and Studio model;
- current implementation status and open design work.

Return to the [OrnaDB frontpage](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
