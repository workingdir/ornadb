# ADR 0052: Raw Calls Select One Object by Unique Text

**Status:** Accepted

## Decision

One canonical non-null Text argument may enter a bounded SERVER `SELECT` that
uses one direct unique Text field as its selector. This is a raw recovery
capability. It does not define general Text predicates or ordinary invocation.

```sql
CREATE SERVER FUNCTION people.by_email (
    p_email TEXT
)
RETURNS ROWS (
    person REF people.person,
    name   TEXT
)
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT REF(selected), selected.name
      FROM people.person selected
     WHERE selected.email = p_email;
```

The accepted function must be active and must have one immutable dedicated
`orna.server-plan` version-4 artifact. It must have exactly:

* one object-type source and one source alias;
* one required, non-null Text parameter;
* the existing work ADR 0048 projection and `ROWS` shape;
* `WHERE source_alias.unique_text_field = text_parameter` in that exact
  operand order; and
* `SERVER`, `SECURITY INVOKER`, `TRANSACTION READ ONLY`, and
  `VOLATILITY STABLE` semantics.

The selected field must be a direct field of the scanned object. Its durable
field declaration must be the exact nullable or required unique Text form
accepted by work ADR 0051. The parameter must have the same exact resolved
Text authority as work ADR 0051. The plan, active catalogue, source object,
field, parameter owner, parameter identity, resolved parameter type, and
definition-reference evidence must agree exactly.

## Source, semantic, and artifact boundary

The current parser reads `source_alias.field = parameter` as two field paths;
it does not read the right-hand name as a parameter. The compiler therefore
reaches only the existing identity-selector branch and rejects the left field
shape. This decision adds one narrow parsed value-selector form. It admits
only a direct field path on the left and one unqualified parameter name on the
right. It does not make either side a general expression or change the syntax
of existing identity selectors.

Syntax errors retain their existing `ORNA0001` code, span, and precedence.
After a query has parsed, the compiler resolves the scanned object, direct
field, declared parameter, projection, and function declarations before it
chooses the selector branch. Existing name, declaration, return-shape, domain,
and function-property diagnostics retain their current precedence. Only an
otherwise resolved direct field-and-parameter equality enters the new
unique-Text semantic check. All other parameterised direct predicates retain
the existing identity-selector rejection.

The compiler records a separate unique-Text query IR. It has no general
parameter expression. Its selector stores the scanned `TypeId`, direct field
owner `TypeId`, direct `FieldId`, parameter owner `FunctionId`, `ParameterId`,
exact resolved Text type, field nullability, and required non-null parameter
fact. Definition-reference evidence retains `QueryObject`, ordered projection
references, `QueryField` for the selected direct field, and `ParameterRead`
for the owner-qualified parameter. It adds no reference kind for equality.

Version 4 is a new, sealed artifact model. It does not reinterpret versions
1, 2, or 3. Its `UniqueTextSelectedServerPlan` contains the existing scan and
projections plus one `SelectBindValue::Text` selector. That selector records
the exact scan `TypeId`, field-owner `TypeId`, `FieldId`, parameter-owner
`FunctionId`, `ParameterId`, resolved Text type, and nullability facts. The
version-4 decoder requires that complete shape, one input-zero scan, a direct
one-step field selector, and no ordering term. It rejects a missing, swapped,
renamed, wrong-owner, wrong-type, nullable-parameter, multi-step, or extra
selector form before PostgreSQL execution.

The raw command remains unchanged:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

Standard input is exactly one complete bounded non-null Text `ORV1` envelope
followed by end of file. Work ADRs 0040 and 0045 remain authoritative for
input shape, command parsing, byte limits, `ParameterId` discovery, status,
and `ORV1` Text bytes. No command, frame, codec, marker, or public event byte
changes in this decision.

## Equality and result

The selected field uses work ADR 0051's private `pg_catalog."C"` collation.
The query compares the supplied Text value with PostgreSQL equality on that
field. Therefore it matches only the same complete UTF-8 byte sequence. It
does not fold case, trim whitespace, change line endings, normalise Unicode,
or use a caller-selected collation.

The required unique constraint proves that one non-null Text argument can
select zero or one row. A nullable field can contain many `NULL` values, but a
required non-null argument cannot equal `NULL`; it therefore selects none of
them. An absent value is an expected zero-row result.

The raw result follows work ADR 0048 exactly:

* zero rows produce `CALL_COMPLETED` with no value event; and
* one row produces one `RESULT_VALUES` action for each projected cell in
  declared projection order, followed by `CALL_COMPLETED`.

Every projected cell must remain in the existing protocol-1 scalar,
typed-null, or object-Reference subset. Existing row, cell, variable-payload,
logical-result, flow-control, cancellation, drain, and resource rules remain
authoritative.

## Protected routing and failure boundary

The server validates only the outer zero-or-one supported argument framing
before it opens PostgreSQL. For an admitted Text envelope, the kernel recovers
one active revision and matching security snapshot, constructs the exact
`InvocationTarget`, and authorises the requested `FunctionId` before it
inspects function domain, signature, parameter, type, field, plan, value, or
object data.

A denied call appends and commits one denied `EXECUTE` audit decision and
returns `EXECUTE_DENIED`. It discloses no target, parameter, field, type,
unique fact, Text value, plan, object, or row fact.

After an allowed decision and its audit append, target selection uses this
order:

1. an accepted raw INSERT candidate;
2. an accepted Reference UPDATE or DELETE candidate;
3. an identity-selected SERVER SELECT version-2 candidate from work ADR 0048;
4. a unique-Text-selected SERVER SELECT version-4 candidate from this ADR;
5. the retained parameter-free raw target path; and
6. unavailable target.

Only a superficial active SERVER `orna.server-plan` version-4 candidate opens
the existing raw SELECT savepoint. Inside that savepoint, the kernel validates
the complete unique-Text target, executes the existing authorised select
entry, and adapts its zero-or-one result. It does not build a second SQL plan,
preflight with another query, retry, or inspect a database error for target
selection.

The execution lowerer accepts only `SelectBindValue::Text`. It requires its
stored scan `TypeId`, field-owner `TypeId`, `FieldId`, parameter-owner
`FunctionId`, `ParameterId`, resolved Text type, and nullability facts to
match the active catalogue and function. It validates one non-null runtime
Text value and binds that value as PostgreSQL `TEXT` to the selected C-collated
field. It does not construct SQL from the value or use a text conversion,
normalisation, or collation expression.

Any signature, artifact, plan, evidence, argument, field, type, result-shape,
or cardinality rejection is a pure target failure. The kernel rolls back the
savepoint, commits the allowed audit, and returns
`PostgresKernelError::RawCallTargetUnavailable { function, rule }`. The raw
adapter maps it to `TARGET_UNAVAILABLE`.

Recovery, catalogue verification, audit, savepoint, query, row decode, result
decode, commit, driver, shutdown, or unknown-outcome failure is operational.
It remains `INTERNAL_FAILURE`. It cannot become `TARGET_UNAVAILABLE`, emit a
partial value, or be hidden by cancellation. A successful read and its allowed
audit commit together. The read changes neither object data nor the active
revision.

## Identity, replay, and restart

The compiler stores the source object, selected unique field, function, and
parameter as stable identities. Source names are discovery evidence only. The
private query uses generated relation and column names and a typed Text bind.

Exact source replay retains the function identity, parameter identity,
selected `FieldId`, artifact, grant, private Text collation, unique constraint,
and stored objects. No regrant is required. A work ADR 0006 semantic field
rename retains the selected `FieldId`; the original discovered function and
parameter identities continue to select by the renamed field. Restart recovery
verifies the active unique-Text physical shape under work ADR 0051 before it
exposes the active revision.

## Required proof

Focused compiler and core proof must establish:

* parsing only the direct field-and-parameter equality while preserving the
  existing syntax and semantic diagnostic precedence for every other shape;
* acceptance only for the exact direct field and one required non-null Text
  parameter with work ADR 0051's version-one and version-two type authority;
* stable owner-qualified field and parameter identities plus exact
  definition-reference evidence;
* a sealed version-4 `SelectBindValue::Text` artifact with exact scan, field,
  parameter, type, and nullability identities, plus byte-exact predicate
  lowering with the stored `pg_catalog."C"` field representation; and
* rejection of every closed query, type, parameter, and field shape before
  private data SQL.

Focused PostgreSQL and direct-boundary proof must establish:

* byte-identical Text selects its one object, while case, whitespace,
  line-ending, and canonically equivalent but byte-distinct values remain
  independent;
* a nullable unique Text field with multiple `NULL` rows selects none for any
  non-null Text argument;
* an absent Text completes without output;
* required and nullable unique Text fields have the same non-null selector
  result rule;
* projections retain exact order and compatible typed NULL values;
* bad `ParameterId`, wrong type, nullable parameter, non-Text parameter,
  non-unique Text field, non-direct field, wrong source, extra parameter,
  wrong artifact or plan, invalid evidence, unsupported result, and a
  cardinality breach are pure target failures after authorisation;
* INSERT, Reference UPDATE or DELETE, and Reference identity SELECT retain
  their routing precedence;
* target failure rolls back the select savepoint and retains one allowed audit;
  and
* concurrent source or security change cannot split decision, target, plan,
  execution, and result across snapshots.

Focused server proof must use the authenticated local socket. It must show
denial before target facts, success, byte-distinct selection, empty completion,
`TARGET_UNAVAILABLE`, `EXECUTE_DENIED`, `INTERNAL_FAILURE` redaction,
flow-control, cancellation, one allowed audit, and private-source retention
without adapter-side catalogue inspection.

The installed proof uses only the built package, checked-in source, public
`/usr/bin/orna` commands, the installed service account, and the local raw
socket. It must create nullable and required unique Text rows, retain multiple
nullable `NULL` rows, read one row by exact bytes, show byte-distinct values
remain separate, complete on an absent value, deny before grant, replay without
regrant, rename the selected field, restart, and reuse the original function
and parameter identities and grants. It must not inspect private relation,
column, constraint, index, or collation identities.

Format, strict Clippy, rustdoc, diff, similarity, workspace, live PostgreSQL,
raw socket, installed package, replay, rename, restart, security-audit,
concurrency, and session-cleanup gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. A focused RED behaviour
tracer precedes the smallest production change that makes it green.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(sql): define raw unique Text reads` | this ADR; `docs/decisions/README.md` | Accept and index the precise raw Text selector, parser, version-4 artifact, equality, routing, security, closure, and proof contract. |
| `feat(syntax): parse unique Text selectors` | `crates/orna-syntax/src/lib.rs`; `crates/orna-syntax/src/parser.rs` | Admit only the direct field-and-parameter equality AST while retaining every other parser closure and diagnostic precedence. |
| `feat(compiler): resolve unique Text selectors` | `crates/orna-compiler/src/relational.rs`; `crates/orna-compiler/src/resolver.rs`; `crates/orna-compiler/src/lib.rs` | Resolve only the exact unique Text selector, retain its identities and evidence, and select the separate query model. |
| `feat(artifact): encode unique Text select plans` | `crates/orna-artifact/src/server_plan.rs`; `crates/orna-compiler/src/relational/artifact.rs`; `crates/orna-compiler/src/prepare.rs` | Define, encode, decode, and prepare the sealed version-4 `SelectBindValue::Text` artifact. |
| `test(postgres): trace raw unique Text reads` | `crates/orna-postgres/tests/server_execution.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Add focused live RED and green target, byte equality, null, routing, audit, rollback, snapshot, and error-classification tracers. |
| `feat(postgres): dispatch raw unique Text reads` | `crates/orna-postgres/src/kernel/server_execution.rs`; `crates/orna-postgres/src/kernel/security.rs` | Validate and dispatch only the accepted version-4 plan inside the existing protected SELECT savepoint. No raw-socket adapter production file changes. |
| `test(server): prove raw unique Text selection` | `crates/orna-server/tests/standard_database.rs` | Prove socket authority, ordered values, redaction, audit, flow control, cancellation, and cleanup. |
| `test(system): exercise installed unique Text selection` | `crates/orna-system-tests/fixtures/product_test_unique_text_select.orna`; `crates/orna-system-tests/fixtures/product_test_unique_text_select_renamed.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove public byte-exact selection, null, absence, denial, grant, replay, rename, restart, identities, and retained authority. |

## Deferred surface

This decision does not accept a general scalar predicate, another comparison
operator or operand order, Text literal, `NULL` argument, nullable/defaulted
parameter, more than one parameter, a second predicate, `AND`, `OR`, `NOT`,
`ORDER BY`, joins, aggregates, grouping, windows, subqueries, common table
expressions, row locking, mutation, a caller-selected collation, case-folded
or normalised search, prefix or substring search, non-unique Text selection,
another scalar, enum, record, constructed value, collection, opaque value,
ORV2 through ORV5, ORF2 through ORF5, `sys.invoke`, presenters, or ordinary
invocation CLI.

It does not change source apply, CLI bytes, ORV1, ORF1, the raw socket, storage
shape, or work ADR 0051's uniqueness semantics.

## Precedence

For this exact non-null direct unique-Text version-4 SERVER SELECT, this ADR
supersedes the conflicting no-scalar-selector and parameter closures in work
ADRs 0009, 0032, 0040, 0045, and 0048. It preserves their existing
identity-selector parsing,
identity, command, codec, frame, transport, authentication, authorisation,
audit, savepoint, cancellation, resource, result, failure-redaction, and all
unrelated target rules.

Work ADR 0051 remains authoritative for Text type authority, byte equality,
nullable unique field behaviour, private collation, unique constraint,
recovery verification, and Text mutation conflicts. This ADR remains a
milestone-4 raw recovery capability. It does not advance or weaken the
constructed-value and `sys.invoke` sequence in work ADRs 0036, 0039, or 0042.
