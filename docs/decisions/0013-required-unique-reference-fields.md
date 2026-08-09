# ADR 0013: Required Unique Reference Fields

**Status:** Accepted

## Decision

The first Orna field-uniqueness form is `UNIQUE` on a required typed reference
in a newly created object type:

```sql
CREATE TYPE tasks.assignment AS OBJECT (
    owner REF tasks.owner NOT NULL UNIQUE
);
```

Modifier order in source does not change the rule. The field must resolve to
an exact `REF target_type`, must be `NOT NULL`, and must be declared `UNIQUE`.
The referenced target must resolve to an object type in the complete candidate
catalogue under the existing reference rules. This permits mutually referring
new object types in one candidate.

For one owner object type and one field, at most one live owner object may
contain a given target `ObjectId`. Equality is exact equality of the opaque
16-byte Orna object identity. The reference's target `TypeId` is already fixed
by the field type. Uniqueness on another field or another owner object type is
an independent constraint.

The field cannot contain `NULL`, so this decision does not define whether two
null values are unique or equivalent. It adds no string collation, numeric
comparison, structural value equality, subtype rule, or PostgreSQL value
semantics.

## Source checking

The parser continues to retain `NOT NULL` and `UNIQUE` losslessly. After name
and type resolution, a `UNIQUE` field outside the accepted shape produces
`ORNA0201` on the complete field declaration with this exact message:

```text
UNIQUE is only available for REF fields that are NOT NULL
```

This includes nullable references, scalar fields, named value types, and
`VOID`. Existing unknown-name, invalid-reference, default-expression, and
`ON DELETE SET NULL` diagnostics retain their current codes, messages, spans,
and precedence. A rejected source bundle produces no deployable revision and
cannot reach private storage changes.

This slice installs uniqueness only when the owning object type is first
created. A later attempt to add or remove uniqueness, add a unique field to an
existing object type, or otherwise change existing physical storage remains an
unsupported migration. Exact source replay and an ADR 0006 semantic field
rename may retain an already installed unique field because neither changes
its stable `TypeId`, `FieldId`, type, nullability, uniqueness, default, delete
action, or physical storage.

## Private PostgreSQL representation

The backend-neutral physical plan retains the field's uniqueness fact. The
PostgreSQL kernel lowers each accepted field to one immediate, non-deferrable,
one-column unique constraint on the generated private column:

```text
constraint: uq_<field-id-hex>
column:     f_<field-id-hex>
```

The constraint and its backing index are private implementation details. The
verifier requires the exact generated name, owner relation, field column,
one-column key, uniqueness, validity, readiness, immediate enforcement, and
absence of a predicate, expression, included column, or non-default null
treatment. It continues to require the existing private primary-key and
foreign-key shapes independently.

Relations and columns are still created before reference foreign keys, so
mutually referring object types retain the apply ordering fixed by ADR 0004.
The unique constraint does not replace the reference foreign key. Protected
schema access and stable generated names remain unchanged.

The durable catalogue already stores and hashes field uniqueness. This
decision needs no catalogue migration, artifact-format change, definition-
reference kind, or public PostgreSQL name. Recovery reconstructs the same
catalogue fact and verifies the exact private constraint and index before the
active revision is trusted.

## Mutation conflicts

`INSERT` and `UPDATE` may write an accepted unique reference field through
their existing typed plans and argument rules. PostgreSQL's unique constraint
is the race-safe authority; OrnaDB does not perform a separate preflight
`SELECT`.

A conflict is recognised only when PostgreSQL returns SQLSTATE `23505` and
names the exact generated `uq_<field-id-hex>` constraint for a required,
unique reference field on the mutation target in the pinned active catalogue.
It produces the typed shared mutation failure `UniqueReferenceConflict`,
retaining the owning mutation target as `owner: TypeId`, the exact
`field: FieldId`, the field's `referenced_type: TypeId`, and the PostgreSQL
error as internal context. The error deliberately does not duplicate the
attempted `ObjectId`, which is already present in the caller's typed argument.
Its public message is:

```text
this reference is already used by another object
```

For both `INSERT` and `UPDATE`, this failure is nested in the existing
contextual `NotCommitted` outcome. The complete transaction rolls back and
`commit_state()` returns `NotCommitted`. OrnaDB does not expose whether the
constraint was checked during the statement or commit. Another `23505`, a
missing or different constraint name, or a constraint that does not match the
pinned active catalogue retains the existing generic failure path for that
execution point.

Updating an object without changing its unique reference, including assigning
the same reference again to that same object, succeeds. Deleting an object
continues to follow the reference field's existing `NO ACTION`, `RESTRICT`, or
`CASCADE` policy. `ON DELETE SET NULL` is unavailable because the field is
`NOT NULL`.

## Required proof

Tests must prove:

* source checking accepts only exact required typed references and rejects
  every other `UNIQUE` field shape with the exact diagnostic and span;
* a new physical plan retains the uniqueness fact and rejects uniqueness
  changes on an existing object;
* PostgreSQL lowering creates the exact stable constraint and one-column
  immediate unique index without changing the reference foreign key;
* physical verification rejects missing, renamed, crossed, non-immediate,
  partial, expression, multi-column, or otherwise unexpected unique
  constraints and indexes;
* apply, recovery, exact replay, and semantic field rename preserve the stable
  `TypeId` and `FieldId`, exact catalogue uniqueness and reference facts, and
  unchanged physical relation, constraint, and index identities;
* the first `INSERT` succeeds, a duplicate reference returns the exact typed
  `NotCommitted` failure, and no rejected row remains;
* `UPDATE` reports the same typed failure and preserves the original object,
  while assigning an object's existing reference to itself succeeds;
* two concurrent transactions trying to claim the same reference produce
  exactly one commit and one typed conflict without partial state;
* unrelated unique violations are not misclassified; and
* hostile `search_path`, transaction cleanup, active-revision pinning, and
  connection cleanup retain their existing guarantees.

Existing `orna.server-mutation-plan` version 1 INSERT, version 2 UPDATE, and
version 3 DELETE payload bytes, decoders, and semantics remain unchanged.
Tests must retain their exact cross-version rejection and prove that existing
mutations and results over non-unique fields behave identically.

## Deferred surface

This decision does not accept:

* nullable unique fields or a public null-uniqueness rule;
* unique scalar, named, value, collection, or composite fields;
* composite, conditional, partial, expression, case-insensitive, deferrable,
  or `NULLS NOT DISTINCT` uniqueness;
* adding, removing, or changing uniqueness on an existing object type;
* user-selected constraint or index names;
* uniqueness across owner object types, fields, databases, or reference target
  types;
* `ON DELETE SET NULL` on a required reference;
* upsert, conflict clauses, caller-selected object identities, automatic
  retries, or a uniqueness preflight query; or
* exposing PostgreSQL constraint, index, collation, operator, or diagnostic
  behaviour as Orna language semantics.

Those features require their own accepted semantics and migration rules.

## Precedence

This record supersedes only ADR 0005's blanket deferral of `UNIQUE` fields for
the required typed-reference shape above. It preserves ADR 0004's private
identity layout, ADR 0006's DDL-free semantic rename rule, ADR 0007's mutation
outcomes, and ADR 0008's delete-policy behaviour.

It narrows the broader uniqueness direction in
`spec/docs/12-object-relational-model.md` and
`spec/docs/22-ddl-reference.md`. It also narrows the nullable
`REF ... UNIQUE` mapping shown in `spec/docs/35-security.md`; nullable unique
references remain deferred. For required unique reference fields on newly
created object types, this accepted record has precedence.
