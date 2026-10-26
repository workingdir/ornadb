# ADR 0007: Single-Object SERVER UPDATE

**Status:** Accepted

## Decision

The first Orna `UPDATE` changes at most one object in a SERVER function:

```sql
CREATE SERVER FUNCTION tasks.update (
    p_task  REF tasks.task,
    p_title TEXT,
    p_done  BOOL,
    p_owner REF tasks.owner
)
RETURNS ROWS (updated REF tasks.task)
SECURITY INVOKER
TRANSACTION ATOMIC
VOLATILITY VOLATILE
AS
    UPDATE tasks.task AS updated
    SET title = p_title, done = p_done, owner = p_owner
    WHERE REF(updated) = p_task
    RETURNING REF(updated);
```

This form is an explicit Orna extension to SQL:2023 Foundation Core. It does
not expose PostgreSQL syntax or make PostgreSQL update behaviour part of the
public contract.

Version 1 of this language form accepts exactly:

* one declared object-type target followed by the mandatory spelling
  `AS target_alias`;
* one non-empty, duplicate-free list of unqualified target-field assignments;
* a declared function parameter, `TRUE`, `FALSE`, or contextually typed `NULL`
  as each assigned value;
* `WHERE REF(target_alias) = selector_parameter`;
* one declared, non-null selector parameter whose type is the exact
  `REF target_type`;
* `RETURNING REF(target_alias)`;
* one non-null `REF target_type` return column in `ROWS (...)`;
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION ATOMIC`, and
  `VOLATILITY VOLATILE` execution semantics.

The declared return-column name is independent of the target alias. The
compiler resolves the target, fields, parameters, selector, and returned
reference to stable typed identities. Source names do not enter the executable
artifact or generated private SQL.

Each assigned value's `ResolvedType` must exactly equal its target field type.
A REF must carry the exact target `TypeId`; this slice performs no implicit
casts or coercions. All declared parameter types, and every explicit `NULL`
assignment type, use the runtime subset accepted by ADR 0005. Every parameter
is required and non-null. An explicit `NULL` is valid only for a nullable
target field.

The selector is identity-based. It does not expose or depend on a public
primary-key field. The private `_orna_object_id` primary key proves that the
statement can match no more than one object.

## Result

The result has zero or one row:

* If the selected object does not exist, the transaction commits and returns
  an empty `ResultRows` value.
* If the selected object exists, the transaction commits and returns one
  non-null `REF target_type` value with the exact input `ObjectId`.

An absent target is an expected result, not an error. The runtime rejects more
than one returned row or a different returned object identity as a private
kernel invariant failure.

## Definition-reference evidence

The update reuses the durable reference kinds added for INSERT. It does not
require a PostgreSQL migration or a new definition-reference kind.

After signature references, body evidence uses this exact source order:

1. `WriteObject` for the update target;
2. `WriteField` for each `SET` target, followed by `ParameterRead` when that
   assignment reads a parameter;
3. `ObjectReference` for `REF(target_alias)` in the selector;
4. `ParameterRead` for the selector parameter;
5. `ObjectReference` for `REF(target_alias)` in `RETURNING`.

Operation identity belongs in the executable artifact, not in the reference
kind vocabulary.

## Artifact boundary

Existing INSERT artifacts keep the exact `orna.server-mutation-plan` version 1
bytes defined by ADR 0005. UPDATE uses version 2 of that format. Version 2 adds
operation tag 2 and stores:

* the target `TypeId`;
* the selector `FunctionId` and owner-qualified `ParameterId`;
* the ordered field assignments;
* the returned object `TypeId`.

The language identity remains `orna.language/1`. The runtime decodes version 1
INSERT and version 2 UPDATE separately and requires the stored artifact version
to equal the payload version. It does not reinterpret a version 1 artifact.

## Execution and commit outcome

The update executes in one read-write, repeatable-read transaction pinned to
one active revision. The runtime validates the active function, immutable
revision, artifact, catalogue, definition-reference evidence, arguments,
selector, and assignment types before it sends private data SQL.

The private statement uses only stable generated table and column names. It
updates one row by `_orna_object_id` and returns that private identity for
exact result validation.

A statement or deferred-integrity failure rolls back and is known not to have
committed. A transport failure during `COMMIT` has an unknown outcome. That
error carries the function context, target `TypeId`, selector `ObjectId`, and
whether the statement matched an object. Callers must not retry it
automatically because a retry can change one returned row into no rows.

If PostgreSQL confirms the commit and connection shutdown then fails, the
outcome remains committed and the error carries the complete zero-or-one-row
result.

## Deferred surface

This decision does not accept:

* an update without `AS target_alias`;
* an empty `SET` list, duplicate target fields, or qualified target fields;
* a general predicate, another identity comparison, or more than one target;
* a second row, `FROM`, a subquery, a function call, or a general expression;
* another `RETURNING` shape;
* parameter defaults, nullable parameters, or implicit casts;
* updates to private object identity;
* updates that also change field defaults, uniqueness, or physical layout;
* procedural mutation bodies, top-level mutation, authorisation, invocation
  IDs, idempotent retries, or protocol streaming;
* `DELETE`, merge, or cascade invocation semantics.

## Precedence

This record removes only `UPDATE` from the deferred surface in ADR 0005. It
narrows the broader mutation and procedural examples in the project
specification. For this single-object SERVER update, this accepted record has
precedence.
