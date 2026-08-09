# ADR 0011: Direct Boolean SERVER SELECT Predicates

**Status:** Accepted

## Decision

The first Orna search condition that does not use equality is a direct Boolean
predicate in a parameter-free, duplicate-preserving SERVER query:

```sql
CREATE SERVER FUNCTION tasks.active()
RETURNS ROWS (task REF tasks.task)
AS
    SELECT REF(task)
      FROM tasks.task AS task
     WHERE task.active;
```

This form makes the SQL Boolean search-condition rule concrete without adding
general expression syntax. It uses Orna type and NULL semantics and does not
inherit unrelated PostgreSQL operators or behaviour.

The accepted predicate is exactly one of:

* `TRUE` or `FALSE`;
* a source-rooted field path whose final resolved type is `BOOLEAN`.

The field path may traverse existing typed references. Its nullability is
computed from every field-path step using the existing query rules. The final
field may itself be nullable.

The function must have zero declared parameters. It otherwise uses the
existing version-1 SERVER SELECT signature, projection, optional `ORDER BY`,
result-shape, limit, transaction, and execution rules. Existing equality
predicates are unchanged.

## Search-condition result

A row is included only when the predicate evaluates to `TRUE`.

* `TRUE` includes the row;
* `FALSE` excludes the row;
* `NULL` excludes the row.

`NULL` may come from a nullable final Boolean field or from any absent value in
a nullable reference path. This is the SQL `UNKNOWN` search-condition outcome;
it is not equal to either Boolean value and does not change the stored or
returned value.

The predicate only filters rows. It does not change their shape, values,
duplicate-preserving behaviour, or ordering. `WHERE TRUE` therefore retains
every source row, while `WHERE FALSE` returns no rows.

## Diagnostics and precedence

The parser accepts the two direct predicate forms above in an implicit-`ALL`
SELECT. A source-rooted path whose final type is not `BOOLEAN` reaches semantic
checking and uses the existing diagnostic on the complete predicate:

```text
WHERE requires a BOOLEAN expression
```

Unknown aliases, unknown fields, invalid reference traversal, and all existing
expression errors retain their current diagnostic codes, messages, spans, and
precedence.

A parsed direct form outside the accepted field-or-literal boundary, such as
`WHERE REF(task)`, produces `ORNA0001` on the complete predicate with:

```text
WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate
```

A direct field or Boolean literal after `SELECT DISTINCT` produces `ORNA0001`
on the complete predicate with:

```text
SELECT DISTINCT WHERE must use an equality predicate
```

A parameterised SELECT still requires the exact identity selector accepted by
ADR 0009. A direct predicate in a function that declares a parameter is not
reinterpreted as an identity selector and fails through that existing closed
function boundary.

The following remain syntax errors rather than being passed through to
PostgreSQL:

* `NOT`, `AND`, or `OR`;
* `IS NULL`, `IS TRUE`, `IS FALSE`, or `IS UNKNOWN`;
* comparisons other than the existing equality form;
* parenthesised, arithmetic, call, aggregate, subquery, or CASE expressions;
* a parameter read as a direct predicate.

## Definition-reference evidence

This form reuses the existing query evidence. It requires no new durable
reference kind or PostgreSQL migration.

After signature references, evidence remains in this order:

1. `QueryObject` for the source object;
2. projection evidence in projection order;
3. predicate evidence;
4. ordering evidence in ordering-expression order.

A direct field-path predicate records one `QueryField` for every path step, in
path order. A Boolean literal records no definition reference. The predicate
operation itself records no reference.

Preparation must now independently replay version-1 field ownership, reference
targets, final type, cumulative nullability, predicate facts, and the complete
ordered evidence sequence against the candidate catalogue before encoding the
artifact. This closes an existing version-1 trust-boundary gap rather than
assuming every private checked value is internally consistent. Runtime
continues to replay the same facts against the recovered active catalogue
before issuing private data SQL.

## Artifact and revision boundary

The direct predicate uses the existing `orna.server-plan` version 1 model and
bytes. `FieldPath` and `BooleanLiteral` already have version-1 expression tags,
and version 1 already carries one optional typed Boolean selection. This record
adds no expression tag, plan field, format version, language version, core
type, result type, or public execution method.

Existing version-1 artifacts retain their exact bytes and semantics. Adding or
removing a predicate changes the checked query, artifact payload and content
hash, function semantic hash, and immutable function revision. A source-only
formatting or trivia change may advance the active source revision while
reusing the same immutable function revision and artifact.

Identity-selected version 2 remains limited to its exact
`REF(source_alias) = selector_parameter` predicate. Parameter-free
`SELECT DISTINCT` source and compiler construction remain limited to their
existing optional equality predicate. The version-3 artifact deliberately
stores a version-1-compatible optional Boolean selection, and its decoder and
runtime continue to accept every structurally valid selection in that model;
this record neither relies on nor changes that private forward-compatible
boundary. No version-2 or version-3 compiler route, artifact bytes, runtime
behaviour, or diagnostic for a previously accepted query is changed by this
record. Newly parsed but rejected direct forms use the diagnostics specified
above.

## Execution boundary

Execution remains one read-only repeatable-read operation pinned to one active
revision. Before data access, the runtime validates the artifact, function
signature, scan, projections, predicate type and nullability, field-path
ownership, reference targets, definition-reference evidence, and result shape.

The private statement lowers the checked field path or Boolean literal using
only stable generated relation and column names. PostgreSQL's private Boolean
search-condition operation is acceptable only because the result described
above is fully specified by Orna. No PostgreSQL source syntax, identifier,
diagnostic, collation, cast, or additional truth rule becomes public.

Existing row, cell, value, payload, expression, join, statement, and timeout
limits apply unchanged. Rejected functions issue no private data SELECT.

## Required proof

Tests must prove:

* lossless parsing and exact source spans for a direct field path and both
  Boolean literals;
* `SELECT DISTINCT` and identity-selected queries retain their prior closed
  predicate boundaries;
* source-level checking accepts non-null and nullable Boolean paths and rejects
  non-Boolean paths with the existing exact diagnostic;
* predicate field references retain exact owner-qualified identities, order,
  logical paths, and source spans;
* preparation emits and decodes canonical version-1 bytes, retains the exact
  predicate facts and evidence, and both the version-2 and version-3 decoders
  reject those version-1 bytes;
* source-only replay reuses the immutable function revision;
* live PostgreSQL execution includes only `TRUE` rows and excludes both
  `FALSE` and `NULL`, including `NULL` reached through a nullable reference;
* duplicate rows and existing ordering retain version-1 behaviour;
* hostile `search_path`, tampered artifacts or evidence, rollback, session
  cleanup, and snapshot pinning retain their existing guarantees.

## Deferred surface

This record does not accept:

* source/compiler construction of direct predicates in version-2
  identity-selected or version-3 DISTINCT queries;
* parameters, joins, grouping, aggregates, windows, common table expressions,
  subqueries, source `LIMIT`, or source `OFFSET`;
* explicit `SELECT ALL`;
* new Boolean, comparison, NULL-test, arithmetic, string, numeric, temporal,
  call, or general expression forms;
* changes to result limits, streaming, authorisation, invocation, presenters,
  or protocol behaviour.

Those features require their own accepted semantics rather than inheriting
PostgreSQL behaviour.

## Precedence

This record makes one SQL:2023 Boolean search condition concrete under
ADR 0002. It preserves the version and selector boundary in ADR 0009 and the
DISTINCT boundary in ADR 0010. It narrows the broader relational direction in
`spec/docs/12-object-relational-model.md`,
`spec/docs/25-source-compiler-ir.md`, and `spec/docs/39-testing.md`.

For a direct Boolean predicate in a parameter-free duplicate-preserving SERVER
SELECT, this accepted record has precedence.
