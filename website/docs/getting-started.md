---
title: Getting started with OrnaDB
description: The object-relational program model in five minutes. Types, functions, invocation, and the CLIENT and SERVER boundary.
---

# Getting started with OrnaDB

OrnaDB is an object-relational database that stores and runs the programs which work with its data. Types describe values and durable objects. Functions describe all executable behaviour. Invocations create running programs.

:::warning Development status
OrnaDB is under active development. There is no released executable yet. Examples on this site show the intended language. The [status page](/status/) separates implemented work from locked design, current proposals, and open questions.
:::

## The model

Four ideas carry the whole system:

1. Types describe data. Object types are durable and identity-bearing. Value types are passed by value.
2. Functions describe every executable behaviour. There is no separate definition kind for queries, procedures, components, screens, or applications.
3. Every function declares one domain: SERVER or CLIENT. SERVER functions run beside the data. CLIENT functions run in the local `orna` process.
4. Invocations create running programs. A root invocation is planned by `sys.invoke` and delivered to a client surface.

## Step 1. Define data

```sql
CREATE TYPE people.person AS OBJECT (
    name  TEXT NOT NULL,
    email TEXT UNIQUE
);

CREATE TYPE tasks.task AS OBJECT (
    title     TEXT NOT NULL,
    assignee  REF people.person,
    due_at    TIMESTAMP,
    completed BOOL NOT NULL DEFAULT FALSE
);
```

`REF people.person` is a typed reference to an object identity. It is not an untyped foreign key. The [object model](/object-model/) page explains the full type system.

## Step 2. Define a function

A query becomes a SERVER function with a SQL body:

```sql
CREATE SERVER FUNCTION tasks.overdue()
RETURNS SET OF REF tasks.task
AS
    SELECT REF(t)
        FROM tasks.task t
        WHERE t.completed = FALSE
          AND t.due_at < sys.time.now();
```

## Step 3. Invoke it

```bash
orna invoke tasks.overdue
```

An interactive terminal normally receives a table. Automation can require a machine representation:

```bash
orna invoke tasks.overdue --output json
```

The target function does not change. Presentation is planned separately from execution. See [invocation](/invocation/).

## Step 4. Return a UI value

A CLIENT function can return `std.ui.UI`, a standard-library value type. The
local client selects an installed runtime to materialise it:

```sql
CREATE CLIENT FUNCTION tasks.overdue_window()
RETURNS std.ui.UI
AS
    std.ui.window(
        title   => 'Overdue Tasks',
        content => std.ui.text(
            text => 'Review overdue tasks'
        )
    );
```

```bash
orna invoke tasks.overdue_window
```

This static UI constructor path is accepted. Scalar and `STREAM<T>` resources
are also accepted with explicit `AWAIT`; `TABLE`/`ROWS` resource transport and
the virtual model APIs remain deferred. See [UI and runtimes](/ui-and-runtimes/)
for the bounded runtime profile.

## What runs where

| Piece | Runs in | Responsibility |
|---|---|---|
| SERVER function | OrnaDB server | SQL, transactions, durable objects, security |
| CLIENT function | `orna` client process | UI values, local state, calls to SERVER functions |
| Presenter | server or client | transforms a result into a sink type |
| Runtime | local machine | materialises terminal, native, or web output |
| `sys.invoke` | server | plans every root invocation |

## Next steps

- Read the [object model](/object-model/).
- Read the [function language](/functions/).
- Read the [invocation system](/invocation/).
- Run through the [examples](/examples/).
- Check what exists today on the [status page](/status/).

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
