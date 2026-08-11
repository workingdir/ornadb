---
title: Object model
description: Object types, value types, typed REF<T> references, collections, and how the logical model maps to SQL.
---

# Object model

The data model has two broad categories. Object types are durable and identity-bearing. Value types are passed by value and have no inherent object identity.

:::warning Development status
The logical model is LOCKED. The physical storage layout is OPEN. OrnaDB is under active development; there is no released executable yet.
:::

## Object types

An object type declares named, typed fields. Every durable object has an `ObjectId`, a `TypeId`, field values, and revision metadata.

```sql
CREATE TYPE people.person AS OBJECT (
    name     TEXT NOT NULL,
    birthday DATE,
    email    TEXT UNIQUE
);

CREATE TYPE crm.organisation AS OBJECT (
    name TEXT NOT NULL UNIQUE
);

CREATE TYPE crm.contact AS OBJECT (
    person       REF people.person NOT NULL,
    organisation REF crm.organisation,
    notes        TEXT
);
```

The logical identity is not "row 17 in a named table". A SQL projection may expose the type as a relation, but references target object identity.

## Value types

A value type is passed by value. It does not inherently have an `ObjectId`.

Examples include `TEXT`, `DECIMAL`, `TIMESTAMP`, `std.json.Value`, and `std.ui.UI`. Some value types are persistable. Others are transient or opaque.

The exact `AS VALUE` DDL is a CURRENT PROPOSAL. Conceptually, a standard module might declare:

```sql
CREATE TYPE std.ui.UI AS VALUE
    OPAQUE
    IMMUTABLE
    TRANSIENT;
```

Core supports the generic value-type facility. It does not contain UI-specific branches.

| Category | Has `ObjectId` | Persisted | Example |
|---|---|---|---|
| Object type | yes | durable | `crm.customer` |
| Value type | no | varies | `TEXT`, `std.ui.UI` |

## Typed references

`REF T` guarantees that the referenced identity is a `T` or an accepted subtype, if subtype semantics are enabled.

```sql
SELECT
    c.person.name,
    c.organisation.name
FROM crm.contact c;
```

The parser resolves each path segment to a stable field ID:

```text
c.person            -> FieldId(crm.contact.person)
.person target type -> TypeId(people.person)
.name               -> FieldId(people.person.name)
```

The runtime does not keep fragile strings after resolution.

The SQL function `REF(alias)` obtains the typed reference to the object scanned as that alias:

```sql
SELECT REF(c)
    FROM crm.contact c;
```

## Collections

Current type constructors:

```text
LIST<T>    ordered, duplicates allowed
SET<T>     unordered, logically unique
MAP<K,V>   key/value pairs
REF<T>     identity-bearing typed reference
OPTION<T>  nullable form, exact syntax open
STREAM<T>  execution-time stream
```

Physical storage may normalise collections or use native array-like representations, as long as the logical semantics hold.

## Null and missing

V1 should avoid a separate "missing property" state for declared object fields. A field is present and either has a value or is nullable. Dynamic document-like values belong in explicit value types such as `std.json.Value`.

## Delete policies

```sql
person REF people.person ON DELETE RESTRICT
organisation REF crm.organisation ON DELETE SET NULL
```

`CASCADE` must be explicit and inspectable. The catalog stores the resolved target field ID and the delete policy.

## Inheritance and traits

OPEN. Class-style inheritance is not required merely because the product is object-relational. A first version can support named object types and typed references without subtyping.

## SQL interoperability

The goal is familiar SQL:

```text
SELECT, INSERT, UPDATE, DELETE, JOIN, GROUP BY, ORDER BY,
CTEs, window functions
```

Object conveniences are additive:

```text
REF(alias)
typed path dereference
object and type metadata in sys.*
```

PostgreSQL wire compatibility is a desirable implementation direction, but it is not yet a locked requirement.

## Physical model

OPEN. The recommendation is to prototype over PostgreSQL tables and columns or a typed sparse layout before considering a custom engine. A naive table of `(object_id, field_id, value)` creates poor query planning and indexing.

## Next steps

- Read how types are used by the [function language](/docs/functions/).
- Read how results travel through [invocation](/docs/invocation/).
- Check the [glossary](/docs/glossary/) for term definitions.

Return to the [OrnaDB frontpage](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
