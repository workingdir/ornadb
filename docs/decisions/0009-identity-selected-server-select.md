# ADR 0009: Identity-Selected SERVER SELECT

**Status:** Accepted

## Decision

The first parameterised Orna `SELECT` selects at most one source-object row in
a SERVER function:

```sql
CREATE SERVER FUNCTION tasks.get (
    p_task REF tasks.task
)
RETURNS ROWS (
    task  REF tasks.task,
    title TEXT,
    done  BOOL
)
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT REF(selected), selected.title, selected.done
      FROM tasks.task selected
     WHERE REF(selected) = p_task;
```

This form uses the existing Orna relational `SELECT` and typed `REF(object)`
semantics. It does not expose PostgreSQL syntax or make PostgreSQL parameter or
query behaviour part of the public contract.

Version 1 of this language form accepts exactly:

* one declared object-type source followed by one source alias, with optional
  `AS` before the alias;
* the existing supported non-empty `SELECT` projection list and matching
  non-empty `ROWS (...)` declaration;
* exactly one declared function parameter;
* one required, non-null selector parameter whose type is the exact
  `REF source_type`;
* `WHERE REF(source_alias) = selector_parameter` with that exact operand order;
* no `ORDER BY` clause;
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION READ ONLY`, and
  `VOLATILITY STABLE` execution semantics.

The compiler resolves the source object, projections, selector, and parameter
to stable typed identities. The selector is stored as the owner-qualified pair
of the function's `FunctionId` and the parameter's `ParameterId`. Source names
do not enter the executable artifact or generated private SQL.

The selector parameter's `ResolvedType` must be the exact `REF source_type`.
The supplied runtime value must carry the same target `TypeId`. This slice
performs no implicit casts or coercions. The function has no parameter default,
and the caller must supply one non-null value for the exact `ParameterId`.

The selector is identity-based. It does not expose or depend on a public
primary-key field. The private `_orna_object_id` primary key proves that the
statement can match no more than one object.

## Result

The result has zero or one row:

* If the selected object does not exist in the pinned snapshot, the read-only
  transaction commits and returns an empty `ResultRows` value.
* If the selected object exists and result validation succeeds, the read-only
  transaction commits and returns one row with the exact declared `ROWS (...)`
  shape.

An absent target is an expected result, not an error. The runtime rejects more
than one returned row as a private kernel invariant failure. Existing result
column type, nullability, row-width, value, and payload validation continues to
apply unchanged.

## Definition-reference evidence

The identity-selected query reuses the durable reference kinds already used by
SERVER queries and mutations. It does not require a PostgreSQL migration or a
new definition-reference kind.

After signature references, body evidence uses this exact order:

1. `QueryObject` for the `FROM` source object;
2. projection evidence in projection order, with `ObjectReference` for each
   `REF(source_alias)` and `QueryField` for each field-path step;
3. `ObjectReference` for `REF(source_alias)` in the selector;
4. `ParameterRead` for the owner-qualified selector parameter.

The equality operation itself adds no definition reference. Parameter
ownership and operation shape belong in the executable artifact, not in the
reference-kind vocabulary.

## Artifact boundary

Existing no-argument `orna.server-plan` version 1 artifacts keep their exact
bytes and semantics. They are not re-encoded or reinterpreted.

The identity-selected query uses `orna.server-plan` version 2. Version 2 keeps
the version-1 scan, projection, selection, and ordering structures and adds one
parameter expression. That expression stores:

* the selector function's `FunctionId`;
* the owner-qualified selector `ParameterId`;
* the parameter's exact resolved `REF source_type` and non-nullability.

The version-2 selection must be one equality whose left operand is the
reference to input zero and whose right operand is that parameter expression.
The version-2 plan must have no parameter expression in a projection or
ordering term and must have no ordering terms. The active function must declare
exactly the same owner-qualified parameter and type.

The language identity remains `orna.language/1`. The runtime selects the exact
decoder from the stored artifact version and requires the durable artifact
version and payload version to agree. It does not decode a version-1 artifact
with version-2 rules or change any version-1 expression tag.

## Execution and snapshot outcome

The query executes in one read-only, repeatable-read transaction pinned to one
active revision. Within that same snapshot, the runtime validates the active
function, immutable revision, artifact, catalogue, definition-reference
evidence, complete argument set, parameter ownership, and exact reference type
before it sends private data SQL.

Argument validation rejects a duplicate argument, an unknown `ParameterId`, a
missing argument, a null value, an unsupported runtime type, an inactive REF
target, a wrong REF target, or a value whose resolved type differs from the
declared parameter type. No rejected argument reaches private data SQL.

The private statement uses only stable generated relation and column names. It
compares the supplied `ObjectId` with `_orna_object_id` through a typed bind.
Query, result-validation, commit, or connection-shutdown failure cannot change
durable object data or the active revision pair.

The runtime does not retry a failed read automatically. If PostgreSQL confirms
the commit and connection shutdown then fails, the invocation returns a
contextual execution error rather than the collected result. A caller may
start a new invocation, but that invocation may pin a later active revision or
observe a later data snapshot.

## Deferred surface

This decision does not accept:

* a parameterised query without the exact identity predicate;
* another operand order, predicate, identity comparison, or more than one
  selector;
* more than one function parameter, a scalar selector, a parameter default, a
  nullable parameter, a null argument, an implicit cast, or a coercion;
* parameter expressions in projections or ordering terms;
* `ORDER BY`, a second object source, a join, subquery, common table expression,
  function call, aggregate, grouping, window operation, or general expression;
* object existence as an error or a caller-selected public primary key;
* row locking, mutation, procedural bodies, authorisation, invocation IDs,
  protocol streaming, presenter selection, or CLI argument conversion.

Those features require their own accepted semantics rather than inheriting
PostgreSQL behaviour.

## Precedence

This record narrows the typed argument and SQL invocation direction in
`spec/docs/11-function-program-model.md`, `spec/docs/13-invocation-system.md`,
and `spec/docs/38-implementation-roadmap.md`. It makes one parameterised use of
the typed object-reference model in `spec/docs/12-object-relational-model.md`
concrete. It does not define authentication, `sys.invoke`, public protocol, or
CLI behaviour. For this identity-selected SERVER query, this accepted record
has precedence.
