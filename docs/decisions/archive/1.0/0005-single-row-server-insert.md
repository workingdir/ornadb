# ADR 0005: Single-Row SERVER INSERT

**Status:** Accepted

## Decision

The first Orna mutation body is one single-row `INSERT` in a SERVER function:

```sql
CREATE SERVER FUNCTION tasks.create (
    p_title TEXT,
    p_done  BOOL,
    p_owner REF tasks.owner
)
RETURNS ROWS (created REF tasks.task)
SECURITY INVOKER
TRANSACTION ATOMIC
VOLATILITY VOLATILE
AS
    INSERT INTO tasks.task AS created (title, done, owner)
    VALUES (p_title, p_done, p_owner)
    RETURNING REF(created);
```

This form is an explicit Orna extension to SQL:2023 Foundation Core. It does
not expose PostgreSQL syntax or make PostgreSQL `RETURNING` behaviour part of
the public contract.

Version 1 accepts exactly:

* one declared object-type target followed by the mandatory spelling
  `AS target_alias`;
* one explicit, non-empty, duplicate-free list of unqualified target-field
  identifiers;
* one non-empty `VALUES` row with the same arity as the field list, mapped to
  fields positionally;
* parameter reads, `TRUE`, `FALSE`, and contextually typed `NULL` as values;
* one `RETURNING REF(target_alias)` expression;
* one non-null `REF target_type` return column in `ROWS (...)`;
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION ATOMIC`, and
  `VOLATILITY VOLATILE` execution semantics.

The declared return-column name is independent of the target alias. The
compiler resolves the target, fields, parameters, and returned reference to
stable typed identities. Source names do not enter the executable artifact or
generated private SQL.

Each value's `ResolvedType` must exactly equal its paired field type. A REF
must carry the exact target `TypeId`; this slice performs no implicit casts or
coercions. All declared function parameter types, and every explicit `NULL`
assignment type, are limited to semantic `BOOLEAN`, `INTEGER`, `BIGINT`,
`FLOAT`, `CHARACTER LARGE OBJECT`, `BINARY LARGE OBJECT`, and exact typed
`REF`. The source aliases `BOOL`, `INT`, `TEXT`, and `BYTES` resolve to those
same semantic types. Boolean literals may only target `BOOLEAN` fields.

After every semantic, argument, artifact, and active-revision check succeeds,
the server allocates an opaque `ObjectId`. Callers cannot supply, predict, or
replace it. A successful invocation returns exactly one result row whose sole
value is the typed reference containing that identity.

Every function parameter is required and non-null in this slice. An explicit
`NULL` is valid only for a nullable target field. An omitted nullable field is
stored as null. Omitting a mandatory field is a compiler error. Fields with
defaults and `UNIQUE` fields remain rejected by physical planning; mutation
execution does not emulate either feature.

The mutation executes in one read-write atomic transaction pinned to one
active revision. Wrong-target references fail before private SQL. A missing
same-target object is rejected by the private foreign key and the transaction
rolls back. No partial result is returned.

The normal result is returned only after PostgreSQL confirms the commit. If
the transport is lost while the commit outcome is unknown, the error carries
the candidate `ObjectId` and explicitly forbids automatic retry. If commit is
confirmed but connection shutdown then fails, the outcome remains committed
and the error carries the committed result. These states must not collapse
into an ordinary safe-to-retry execution error.

## Artifact boundary

Mutation functions use a separately versioned `orna.server-mutation-plan`
artifact. They do not extend or reinterpret `orna.server-plan` version 1.
Durable definition-reference evidence distinguishes object and field writes
from reads.

## Deferred surface

This decision does not accept:

* top-level data mutation outside a function;
* multi-row `VALUES`, `INSERT ... SELECT`, `DEFAULT`, or upsert;
* string, numeric, bytes, date/time, and other literal forms, function calls,
  or general expressions beyond the closed value forms above;
* another `RETURNING` shape;
* procedural mutation bodies;
* `UPDATE`, `DELETE`, merge, or cascade invocation semantics;
* caller-provided object identities;
* parameter defaults, nullable parameters, authorisation, invocation IDs,
  idempotent retries, or protocol streaming.

Those features require their own accepted semantics rather than inheriting
PostgreSQL behaviour.

## Precedence

This record narrows the general mutation direction in
`spec/docs/12-object-relational-model.md`, the procedural mutation examples in
`spec/docs/05-execution-locations.md`, `spec/docs/22-ddl-reference.md`, and
`spec/docs/23-function-language.md`, and the top-level seed examples in
`spec/examples/09_presenters.orna` and `spec/examples/10_launch_entries.orna`.
For the first executable mutation slice, this accepted record has precedence.
