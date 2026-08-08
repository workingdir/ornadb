# ADR 0010: Parameter-Free SERVER SELECT DISTINCT

**Status:** Accepted

## Decision

The first Orna duplicate-eliminating query is a parameter-free SERVER function
using `SELECT DISTINCT`:

```sql
CREATE SERVER FUNCTION tasks.completion_values()
RETURNS ROWS (completed BOOL)
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT DISTINCT task.completed
      FROM tasks.task AS task;
```

This form makes the accepted `ROWS` duplicate contract concrete. It uses Orna
relational semantics and does not expose or inherit PostgreSQL collation,
floating-point, ordering, or duplicate-elimination behaviour.

The first supported form accepts exactly:

* `DISTINCT` immediately after `SELECT`;
* one declared object-type source followed by one source alias, with optional
  `AS` before the alias;
* the existing supported non-empty projection list and matching non-empty
  `ROWS (...)` declaration;
* only projections whose resolved semantic type is `BOOLEAN`, `INTEGER`,
  `BIGINT`, `BINARY LARGE OBJECT`, or an exact typed `REF`;
* nullable and non-null projections in that type domain;
* the existing optional parameter-free equality predicate;
* no declared function parameters;
* no `ORDER BY` clause;
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION READ ONLY`, and
  `VOLATILITY STABLE` execution semantics.

Every projection must independently satisfy the DISTINCT type domain. A
multi-column result is supported when every projected column satisfies it.
Projection expressions and return columns must continue to agree exactly in
type. Expression nullability must match the active field path and becomes the
runtime result-column nullability; `ROWS (...)` declarations do not add a
separate nullability fact.

## Row equivalence

`DISTINCT` partitions the shaped result into equivalent rows and returns one
row from each equivalence class.

Two rows are equivalent only when they have the same width and every pair of
columns is equivalent. For one column:

* two `NULL` values of the same resolved type are equivalent;
* `NULL` and a non-null value are not equivalent;
* two `BOOLEAN` values are equivalent when their truth values are equal;
* two `INTEGER` or two `BIGINT` values are equivalent when their numeric values
  are equal;
* two `BINARY LARGE OBJECT` values are equivalent when their octet sequences
  have the same length and contents;
* two typed references are equivalent when they have the same target `TypeId`
  and the same opaque `ObjectId`.

This NULL rule is duplicate-elimination equivalence. It does not make
`NULL = NULL` true and does not change equality or `WHERE` semantics.

The result contains zero or more rows. It has the exact declared `ROWS (...)`
shape and no observable order. Because equivalent rows expose the same typed
values, the retained physical representative is not observable.

## DISTINCT type domain

The compiler owns a dedicated `supports_server_select_distinct` semantic
predicate. Its initial accepted set is exactly `BOOLEAN`, `INTEGER`, `BIGINT`,
`BINARY LARGE OBJECT`, and typed `REF`. Nullability does not change whether a
type is supported. Every other `StandardScalar`, every named type, and every
unresolved type is rejected.

The initial member set intentionally equals the existing SERVER SELECT
equality allowlist, but equality is not the semantic source for DISTINCT. An
implementation must not define DISTINCT support by calling
`supports_server_select_equality`. Preparation reuses the dedicated DISTINCT
predicate, and the artifact and runtime independently validate the same closed
domain. Exhaustive tests cover every `StandardScalar`, typed `REF`, and named
types.

`CHARACTER LARGE OBJECT`, including its `TEXT` alias, is excluded because Orna
has not accepted collation, normalisation, or code-point comparison semantics.

`FLOAT` is excluded even though current runtime values are finite and have
reflexive Rust equality. That runtime value invariant does not define
backend-neutral relational duplicate equivalence, storage comparison, or
future numeric semantics.

A source projection outside the domain produces `ORNA0303` on that complete
projection expression with this exact message:

```text
SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values
```

The compiler reports one such diagnostic for each unsupported projection, in
projection order. It does not report the SERVER SELECT equality diagnostic for
this failure.

An `ORDER BY` clause produces `ORNA0001` on the `ORDER` token with this exact
message:

```text
SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause
```

Function-shape failures use `ORNA0303` and these exact messages:

```text
SELECT DISTINCT SERVER functions require zero declared parameters
SELECT DISTINCT SERVER functions require SECURITY INVOKER
SELECT DISTINCT SERVER functions require TRANSACTION READ ONLY
SELECT DISTINCT SERVER functions require VOLATILITY STABLE
```

Existing syntax, name-resolution, expression-type, return-shape, and equality
diagnostics retain precedence for failures inside the source, projections,
predicate, and `ROWS (...)` declaration.

## Definition-reference evidence

SELECT DISTINCT reuses the existing SERVER query reference kinds. It requires
no PostgreSQL migration and no new definition-reference kind.

After signature references, body evidence uses this exact order:

1. `QueryObject` for the `FROM` source object;
2. projection evidence in projection order;
3. optional predicate evidence.

Within an expression, evidence uses depth-first, left-to-right traversal.
`ObjectReference` records each `REF(source_alias)`. `QueryField` records each
field-path step in path order. Boolean literals and equality operations add no
reference.

`DISTINCT` itself adds no definition reference. Its operation shape belongs in
the checked IR and executable artifact. The `DISTINCT` token has no durable
reference row or reference span.

Preparation validates expression type, field-path ownership, and nullability
against the complete candidate catalogue before it validates the dependency
sequence. The active catalogue is used only for existing-identity continuity.
A count, kind, target, or order difference in that sequence uses this
checked-bundle reason:

```text
SELECT DISTINCT definition references differ from the checked function body
```

Runtime dependency replay remains all-or-nothing. Version 3 uses a distinct
human-facing error path so the mismatch can be explained without exposing the
durable reference-record representation or changing version-1 and version-2
error messages:

```text
saved SELECT DISTINCT function cannot run: its dependencies do not match its signature and query
saved SELECT DISTINCT function cannot run: its dependencies are not in the same order as its signature and query
```

## Artifact boundary

Existing `orna.server-plan` version 1 artifacts retain their exact bytes and
duplicate-preserving semantics. Identity-selected version 2 artifacts retain
their exact bytes, zero-or-one-row semantics, selector model, and public
behaviour. Neither version is re-encoded or reinterpreted.

Parameter-free SELECT DISTINCT uses `orna.server-plan` version 3 and a separate
closed `DistinctServerPlan` model. The model contains:

* one version-1-compatible scan;
* a non-empty version-1-compatible projection list;
* one optional version-1-compatible selection;
* no ordering collection;
* no parameter expression.

Version 3 uses the existing server-plan envelope and the version-1 wire order.
Its header contains version `3`. It encodes the scan, projections, optional
selection, and a mandatory zero ordering count. The artifact version is the
fixed DISTINCT operation marker; version 3 adds no general set-quantifier byte
or Boolean flag.

Version 3 accepts only version-1 expression tags. The private version-2
parameter expression tag is invalid in every version-3 position. Existing
projection, expression-node, field-path, artifact-size, and decoding limits
apply unchanged.

The language identity remains `orna.language/1`. The durable artifact version
and embedded payload version must both be `3`. Each decoder accepts only its
own version, and cross-version decoding fails closed.

## Hashes and revisions

DISTINCT is part of the checked semantic plan. Adding or removing it changes
the artifact version and payload, artifact content hash, function semantic
hash, and immutable function revision. A duplicate-preserving version-1
revision cannot be reused for a version-3 body, even when the current data
contains no duplicates.

A semantically unchanged version-3 function produces the same canonical
artifact bytes and content hash. A source-only formatting or trivia change may
advance the active source revision while reusing the existing immutable
function revision and version-3 artifact. Stable object and field identities
continue to permit replay across accepted renames.

Artifact format, version, payload, payload hash, function semantic hash,
definition-reference evidence, and active revision must remain coupled.
Preparation and recovery reject any disagreement rather than rebuilding or
reinterpreting an artifact.

## Execution and snapshot outcome

The existing `execute_server_select(FunctionId)` operation executes version 3.
No new public invocation method or argument form is introduced.
`execute_server_select_with_arguments` accepts only an empty argument slice for
this function.

Execution uses one read-only, repeatable-read transaction pinned to one active
revision. Within that snapshot, the runtime validates, in order:

1. the active function and immutable revision;
2. the artifact format, durable version, payload version, payload hash, and
   canonical decode;
3. the exact zero-parameter `ROWS` signature and execution modes;
4. the scan, projections, selection, DISTINCT type domain, and active
   catalogue;
5. the complete ordered definition-reference evidence;
6. the generated result shape.

No rejected plan reaches private data SQL.

The private statement uses only stable generated relation and column names and
the fixed `SELECT DISTINCT` operation. DISTINCT applies to the declared Orna
projection before the internal result-row limit. It adds no bind. Existing
typed binds used by accepted version-1 predicate expressions remain unchanged.

Existing row, cell, field-path, expression, SQL, value, and payload limits
apply unchanged. Result type, nullability, row width, value decoding, and
payload validation occur as for version 1.

Query, result-validation, commit, or connection-shutdown failure cannot change
durable object data or the active revision pair. The runtime does not retry a
failed read automatically. If PostgreSQL confirms the read-only commit and
connection shutdown then fails, invocation returns the existing contextual
execution error rather than the collected result.

Runtime function-signature failures use these exact rule strings inside the
existing contextual signature error:

```text
SELECT DISTINCT SERVER functions must have zero parameters
SELECT DISTINCT SERVER functions must return nonempty ROWS
SELECT DISTINCT SERVER functions must use INVOKER security
SELECT DISTINCT SERVER functions must use READ ONLY transactions
SELECT DISTINCT SERVER functions must use STABLE volatility
```

A runtime DISTINCT-domain mismatch uses this exact human-facing message on the
version-3-specific error path:

```text
saved SELECT DISTINCT function cannot run: projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values
```

An ordering term recovered from malformed durable state uses:

```text
saved SELECT DISTINCT function cannot run: ORDER BY is not allowed
```

The version-3-specific error remains nested in the existing contextual SERVER
SELECT execution chain, but its own display text contains no artifact version,
plan-invariant, or definition-reference terminology. Adding the new
non-exhaustive error variant does not change any version-1 or version-2 display
text.

## Deferred surface

This decision does not accept:

* SELECT DISTINCT with a function parameter or argument;
* SELECT DISTINCT in an identity-selected version-2 query;
* `ORDER BY`, source `LIMIT` or `OFFSET`;
* `CHARACTER LARGE OBJECT`, `TEXT`, `FLOAT`, `DECIMAL`, `UUID`, temporal,
  duration, void, named, or other projection types;
* collation, Unicode normalisation, locale-sensitive comparison, approximate
  numeric equivalence, or coercion;
* explicit `SELECT ALL` syntax;
* `DISTINCT ON`, `IS DISTINCT FROM`, or DISTINCT within an aggregate;
* a second source, join, subquery, common table expression, aggregate,
  grouping, window operation, or general expression beyond the existing query
  subset;
* parameter expressions, authorisation, invocation IDs, protocol streaming,
  presenter selection, or CLI conversion;
* changes to version-1 or version-2 artifact bytes, semantics, APIs,
  diagnostics, transaction behaviour, or result limits.

Those features require their own accepted semantics rather than inheriting
PostgreSQL behaviour.

## Precedence

This record makes the accepted duplicate behaviour in
`docs/decisions/0002-public-language-contract.md` concrete for one
parameter-free SERVER query form. It preserves the version boundaries and
runtime rules in `docs/decisions/0009-identity-selected-server-select.md` and
narrows the broader relational direction in
`spec/docs/12-object-relational-model.md`,
`spec/docs/25-source-compiler-ir.md`, and `spec/docs/39-testing.md`.

For parameter-free SELECT DISTINCT, this accepted record has precedence.
