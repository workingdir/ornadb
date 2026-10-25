# ADR 0008: Single-Object SERVER DELETE

**Status:** Accepted

## Decision

The first Orna `DELETE` removes at most one object in a SERVER function:

```sql
CREATE SERVER FUNCTION tasks.remove (
    p_task REF tasks.task
)
RETURNS ROWS (deleted BOOL)
SECURITY INVOKER
TRANSACTION ATOMIC
VOLATILITY VOLATILE
AS
    DELETE FROM tasks.task AS deleted_task
    WHERE REF(deleted_task) = p_task
    RETURNING TRUE;
```

This form is an explicit Orna extension to SQL:2023 Foundation Core. It does
not expose PostgreSQL syntax or make PostgreSQL delete or `RETURNING`
behaviour part of the public contract.

Version 1 of this language form accepts exactly:

* one declared object-type target following `DELETE FROM`, then the mandatory
  spelling `AS target_alias`;
* `WHERE REF(target_alias) = selector_parameter`;
* one declared, non-null selector parameter whose type is the exact
  `REF target_type`;
* `RETURNING TRUE`;
* one non-null `BOOLEAN` return column in `ROWS (...)`;
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION ATOMIC`, and
  `VOLATILITY VOLATILE` execution semantics.

The declared return-column name is independent of the target alias. The
compiler resolves the target and selector to stable typed identities. Source
names do not enter the executable artifact or generated private SQL.

Every declared parameter is required and non-null. Parameter types use the
runtime subset accepted by ADR 0005, and the selector must be an exact
reference to the delete target. Parameter defaults, nullable parameters,
implicit casts, and coercions remain unsupported.

The selector is identity-based. It does not expose or depend on a public
primary-key field. The private `_orna_object_id` primary key proves that the
statement can match no more than one object.

## Result

The result has zero or one row:

* If the selected object does not exist, the transaction commits and returns
  an empty `ResultRows` value.
* If the selected object exists and is deleted, the transaction commits and
  returns one non-null `TRUE` value.

An absent target is an expected result, not an error. The result deliberately
does not return `REF target_type`: a successfully deleted object must not be
represented as a live reference. The caller already supplied its identity.

The private kernel may return the deleted `_orna_object_id` from its private
statement solely to prove that at most one row was deleted and that its
identity equals the selector. That identity is not exposed as the public
result. More than one returned row or a different identity is a private
kernel invariant failure.

## Delete policies

The exact `ON DELETE` policy stored for each referencing field applies in the
same atomic transaction:

* an omitted policy (`NO ACTION`) or `RESTRICT` rejects deletion while a
  dependent reference exists;
* `SET NULL` clears the declared nullable reference field;
* `CASCADE` deletes dependent objects, including further dependants reached
  through explicitly declared cascade policies.

A rejected dependent reference produces an Orna-owned `DeleteRestricted`
failure and a known-not-committed outcome. Orna does not expose PostgreSQL
constraint timing. A successful root deletion returns only its single `TRUE`
row even when explicit `SET NULL` or `CASCADE` policies also change dependent
objects. Any failure rolls back the root deletion and every policy effect.

## Definition-reference evidence

The delete reuses the durable reference kinds already used by INSERT and
UPDATE. It requires no new PostgreSQL migration.

After signature references, body evidence uses this exact source order:

1. `WriteObject` for the delete target;
2. `ObjectReference` for `REF(target_alias)` in the selector;
3. `ParameterRead` for the selector parameter.

`RETURNING TRUE` contains no definition reference. Operation identity belongs
in the executable artifact, not in the reference-kind vocabulary.

## Artifact boundary

Existing `orna.server-mutation-plan` version 1 INSERT and version 2 UPDATE
bytes remain unchanged. DELETE uses version 3 with operation tag 3 and stores:

* the target `TypeId`;
* the selector `FunctionId` and owner-qualified `ParameterId`.

Version 3 has no assignment list or returned-object identity. Its operation
defines the fixed zero-or-one `BOOLEAN` result, which the compiler and runtime
must cross-check against the active function declaration. The language
identity remains `orna.language/1`.

The runtime selects the exact decoder from the stored artifact version and
requires the payload version and operation to agree. It does not reinterpret
version 1 or version 2 artifacts.

## Execution and commit outcome

The delete executes in one read-write, repeatable-read transaction pinned to
one active revision. The runtime validates the active function, immutable
revision, artifact, catalogue, definition-reference evidence, arguments, and
selector before it sends private data SQL.

The private statement uses only stable generated relation names and selects
one row by `_orna_object_id`. A normal statement failure rolls back and is
known not to have committed. A reference-policy rejection is reported as the
typed `DeleteRestricted` failure described above.

A transport failure during `COMMIT` has an unknown outcome. That error carries
the function context, target `TypeId`, selector `ObjectId`, and whether the
statement matched an object. Callers must not retry it automatically: a retry
can change a one-row `TRUE` result into an empty result.

If PostgreSQL confirms the commit and connection shutdown then fails, the
outcome remains committed and the error carries the complete zero-or-one-row
result.

## Deferred surface

This decision does not accept:

* a delete without `FROM` or `AS target_alias`;
* a delete without the exact identity predicate;
* another predicate, identity comparison, target, or `RETURNING` shape;
* returning a reference or any deleted field value;
* a second root row, `USING`, a join, subquery, function call, or general
  expression;
* top-level mutation, soft-delete conventions, archive behaviour, procedural
  mutation bodies, or user-defined delete hooks;
* parameter defaults, nullable parameters, authorisation, invocation IDs,
  idempotent retries, or protocol streaming;
* merge or update-and-delete combinations.

## Precedence

This record removes only this identity-selected `DELETE` and the effects of
already declared `ON DELETE` policies from the deferred surface in ADR 0005.
It narrows the broader SQL direction in
`spec/docs/12-object-relational-model.md`. For this single-object SERVER
delete, this accepted record has precedence.
