# ADR 0012: Direct Boolean Predicates in SELECT DISTINCT

**Status:** Accepted

## Decision

A parameter-free `SELECT DISTINCT` SERVER function may use the direct Boolean
search conditions accepted by ADR 0011:

```sql
CREATE SERVER FUNCTION tasks.active_states()
RETURNS ROWS (active BOOL)
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT DISTINCT task.active
      FROM tasks.task AS task
     WHERE task.visible;
```

The predicate is exactly one of:

* `TRUE` or `FALSE`;
* a source-rooted field path whose final resolved type is `BOOLEAN`;
* an equality expression already accepted for `SELECT DISTINCT`.

The field path may traverse existing typed references. Its nullability is
computed from every field-path step using the existing query rules. The final
field may itself be nullable.

This decision composes two existing Orna query rules. It does not add a
general expression language or inherit another PostgreSQL predicate.

## Filtering and duplicate elimination

The predicate filters source rows before duplicate elimination.

* `TRUE` retains a source row;
* `FALSE` excludes it;
* `NULL` excludes it.

After filtering, `DISTINCT` compares the complete declared projection using
the type domain and equivalence rules fixed by ADR 0010. A nullable Boolean
predicate therefore does not add a result value or a second distinctness
rule. Rows excluded by `FALSE` or `NULL` never participate in duplicate
elimination.

The projection remains limited to `BOOLEAN`, `INTEGER`, `BIGINT`, `BINARY
LARGE OBJECT`, and exact typed `REF`. A direct Boolean predicate may read a
field that is not projected. `ORDER BY` remains unavailable.

## Diagnostics and precedence

The parser accepts the three predicate forms above after `SELECT DISTINCT`.
A direct field path whose final type is not `BOOLEAN` reaches semantic checking
and uses the existing diagnostic on the complete predicate:

```text
WHERE requires a BOOLEAN expression
```

Unknown aliases, unknown fields, invalid reference traversal, projection-domain
errors, and existing equality errors retain their current diagnostic codes,
messages, spans, and precedence.

A parsed direct form outside the accepted field, literal, or equality boundary,
such as `WHERE REF(task)`, produces `ORNA0001` on the complete predicate with:

```text
WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate
```

The following remain syntax errors rather than reaching PostgreSQL:

* `NOT`, `AND`, or `OR`;
* `IS NULL`, `IS TRUE`, `IS FALSE`, or `IS UNKNOWN`;
* comparisons other than the existing equality form;
* parenthesised, arithmetic, call, aggregate, subquery, or `CASE` expressions;
* a parameter read as a direct predicate.

A function that declares any parameter remains outside the parameter-free
`SELECT DISTINCT` boundary. After successful parsing, it produces `ORNA0303`
on the complete function declaration with the existing exact message:

```text
SELECT DISTINCT SERVER functions require zero declared parameters
```

Parser and semantic diagnostics are emitted before preparation. A rejected
query produces no deployable function revision and cannot reach private data
SQL.

## Definition-reference evidence

No new durable reference kind or catalogue migration is required. Evidence
remains in the order fixed by ADR 0010:

1. signature references;
2. `QueryObject` for the source object;
3. projection evidence in projection order;
4. predicate evidence.

A direct field-path predicate records one `QueryField` for every path step, in
path order. A Boolean literal records no definition reference. The predicate
operation itself records no reference.

Preparation independently replays field ownership, reference targets, final
type, cumulative nullability, predicate facts, and the complete ordered
evidence sequence against the candidate catalogue before encoding.

## Artifact and revision boundary

The query uses `orna.server-plan` version 3 unchanged. Version 3 already stores
one optional version-1-compatible Boolean selection and already validates and
encodes `FieldPath` and `BooleanLiteral` expressions. This decision adds no
expression tag, plan field, format version, language version, result type, or
public execution method.

Adding, removing, or changing the predicate changes the canonical artifact
payload and content hash, function semantic hash, and immutable function
revision. A source-only formatting or trivia change may advance the active
source revision while reusing the same immutable function revision and
artifact.

Version-1 duplicate-preserving queries and version-2 identity-selected queries
retain their exact bytes, semantics, diagnostics, and execution paths. ADR
0011 remains authoritative for direct Boolean predicates in version 1.

## Execution boundary

Execution remains one read-only repeatable-read operation pinned to one active
revision. Before data access, the runtime validates the artifact, function
signature, scan, projections, predicate type and nullability, field-path
ownership, reference targets, definition-reference evidence, and result shape.

The private statement uses only stable generated relation and column names.
It applies the checked Boolean search condition before `SELECT DISTINCT`, then
applies the existing result-row limit. PostgreSQL supplies the private Boolean
and duplicate-elimination operations only under the Orna semantics fixed by
ADRs 0010 and 0011. No PostgreSQL identifier, diagnostic, collation, cast, or
additional truth rule becomes public.

Existing row, cell, value, payload, expression, join, statement, and timeout
limits apply unchanged. Rejected functions issue no private data `SELECT`.

## Required proof

Tests must prove:

* lossless parsing and exact source spans for direct field, `TRUE`, and `FALSE`
  predicates after `SELECT DISTINCT`;
* unsupported direct forms retain the exact human-facing diagnostic and span;
* source-level checking accepts non-null and nullable Boolean paths and rejects
  non-Boolean paths with the existing exact diagnostic;
* predicate field references retain exact owner-qualified identities, order,
  logical paths, and source spans after projection evidence;
* preparation emits and decodes canonical version-3 bytes, retains the exact
  predicate facts and evidence, and version-1 and version-2 decoders reject
  those bytes;
* a predicate-only semantic change creates a new immutable function revision,
  while source-only replay reuses the existing revision;
* live execution filters `FALSE` and `NULL` before duplicate elimination,
  including `NULL` reached through a nullable reference;
* predicate fields need not appear in the projection;
* version-1 and version-2 query results remain unchanged;
* hostile `search_path`, active-snapshot pinning, tamper rejection, connection
  cleanup, and read-only behaviour retain their existing guarantees.

## Deferred surface

This decision does not accept:

* a function parameter or runtime argument;
* `ORDER BY`, source `LIMIT`, or source `OFFSET`;
* another projection type or distinctness rule;
* `DISTINCT ON`, `IS DISTINCT FROM`, or DISTINCT within an aggregate;
* a second source, join, subquery, common table expression, aggregate,
  grouping, window operation, or general expression;
* new Boolean, comparison, NULL-test, arithmetic, string, numeric, temporal,
  call, or conditional expression forms;
* changes to result limits, streaming, authorisation, invocation, presenters,
  or protocol behaviour.

Those features require their own accepted semantics rather than inheriting
PostgreSQL behaviour.

## Precedence

This record supersedes ADR 0010's and ADR 0011's deferral of source and compiler
construction for direct Boolean predicates in parameter-free `SELECT DISTINCT`.
It also supersedes ADR 0011's closed parser boundary and exact
`SELECT DISTINCT WHERE must use an equality predicate` diagnostic for direct
field paths and Boolean literals. Version-1 diagnostic copy is unchanged.

This record preserves the prior type, artifact, execution, and compatibility
boundaries and narrows the broader relational direction in
`spec/docs/12-object-relational-model.md`,
`spec/docs/25-source-compiler-ir.md`, and `spec/docs/39-testing.md`.

For a direct Boolean predicate in parameter-free `SELECT DISTINCT`, this
accepted record has precedence.
