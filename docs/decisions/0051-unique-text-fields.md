# ADR 0051: Text Fields Have Byte-Exact Uniqueness

**Status:** Accepted

## Decision

`UNIQUE` is available on nullable and required `TEXT` fields in a newly
created object type:

```sql
CREATE TYPE people.person AS OBJECT (
    email TEXT UNIQUE
);

CREATE TYPE crm.organisation AS OBJECT (
    name TEXT NOT NULL UNIQUE
);
```

For one owner object type and one field, at most one live object can contain a
given non-null Text value. Two Text values are equal for this rule only when
their complete UTF-8 byte sequences are equal. Empty Text is a value. Case,
whitespace, line endings, and Unicode normalisation are not changed before
comparison.

A nullable unique Text field can contain `NULL` in any number of live objects.
`NULL` means that the object has no value for this uniqueness rule. It does not
compare equal to another `NULL`. This decision does not accept `NULLS NOT
DISTINCT`.

Work ADR 0013's required unique Reference form remains unchanged. The complete
accepted source rule is now:

* a nullable or required exact standard Text field; or
* a required typed Reference field.

Uniqueness on another field or owner object type remains independent.

## Exact Text type authority

Source spelling does not define the accepted type. `TEXT` and `CHARACTER LARGE
OBJECT` use the existing standard type resolution rules.

In a catalogue-hash version-one database, the durable field type must be
exactly `ResolvedType::Scalar(StandardScalar::CharacterLargeObject)`. In a
verified catalogue-hash version-two database, the durable field type must be
exactly `ResolvedType::Value(type_id)`, where the pinned standard catalogue
contains that `TypeId` as a `PRIMITIVE`, `IMMUTABLE`, and `PERSISTABLE` value
definition with this exact representation contract:

```text
orna.kernel.value.character-large-object@1
```

Version-one value identities, version-two legacy scalars, source spelling
matches without the required type evidence, and another value contract are
not accepted. Named application types, enum, record, Reference, constructed,
collection, opaque, and every other standard scalar remain outside the Text
branch. Required unique References continue through their separate exact
Reference rule.

## Source checking and migration boundary

The parser continues to retain `NOT NULL` and `UNIQUE` without changing their
order. After name and type resolution, a `UNIQUE` field outside either
accepted shape produces `ORNA0201` on the complete field declaration with this
exact message:

```text
UNIQUE is only available for TEXT fields or REF fields that are NOT NULL
```

Existing unknown-name, invalid-reference, default-expression, and `ON DELETE`
diagnostics retain their codes, messages, spans, and precedence. A rejected
source bundle produces no deployable revision and cannot reach private
storage.

This decision installs Text uniqueness only when the owning object type is
first created. Adding or removing uniqueness, changing the field type,
changing nullability, adding a unique field to an existing object, or changing
the collation remains an unsupported existing-object storage change. Work ADR
0046's appended-field rule continues to reject every unique addition.

Exact replay and a work ADR 0006 semantic field rename retain an installed
unique Text field. Both operations preserve the stable `TypeId`, `FieldId`,
resolved type, nullability, uniqueness, private column, constraint, index, and
collation.

## Private PostgreSQL representation

The backend-neutral physical catalogue retains the normal field uniqueness
fact. It accepts the Text branch only after the exact resolved field type has
projected to
`PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)` through the
selected version-one or verified version-two catalogue authority.

The PostgreSQL kernel lowers a unique Text field to one private column with an
explicit deterministic collation:

```text
f_<field-id-hex> text COLLATE pg_catalog."C"
```

Required fields also use `NOT NULL`. Nullable fields do not. Non-unique Text
columns keep their existing representation.

The kernel adds the existing immediate, non-deferrable, one-column constraint:

```text
constraint: uq_<field-id-hex>
column:     f_<field-id-hex>
```

The constraint and its backing index remain private. The verifier requires the
exact generated relation, column, constraint, and index identities. For a
unique Text column it also requires the `pg_catalog.C` collation, deterministic
collation behaviour, the same exact collation identity in the backing index,
and default distinct-null treatment. It rejects a missing, renamed, crossed,
default-collation, locale-dependent, non-deterministic, non-immediate,
deferrable, partial, expression, included, multi-column, invalid, unready, or
`NULLS NOT DISTINCT` shape.

The durable catalogue already stores and hashes field uniqueness. This
decision adds no catalogue migration, mutation artifact format, public
PostgreSQL name, user-selected collation, or generated comparison key.
Recovery reconstructs the exact catalogue and verifies the private physical
shape before it trusts the active revision.

## Mutation conflicts

Existing accepted `INSERT` and `UPDATE` plans can write a unique Text field.
PostgreSQL's unique constraint is the race-safe authority. OrnaDB does not run
a uniqueness preflight query, normalise the value, retry the statement, or use
an upsert.

A Text conflict is recognised only when PostgreSQL returns SQLSTATE `23505`
and names the exact generated `uq_<field-id-hex>` constraint for an exact
unique Text field on the mutation target in the pinned active catalogue. It
produces this typed private failure:

```text
UniqueTextConflict { owner: TypeId, field: FieldId, source }
```

The failure retains the PostgreSQL error only as internal context. The driver
error can contain PostgreSQL diagnostic detail, including the rejected value.
OrnaDB does not copy that detail into another error field, public frame, audit
record, or stable display. The protected raw socket diagnostic writes only the
stable Orna error display and does not serialise the driver source chain. Its
stable message is:

```text
this text value is already used by another object
```

`UniqueReferenceConflict` remains unchanged. Another `23505`, a missing or
different constraint name, or a constraint that does not match the pinned
active catalogue retains the generic database failure for that execution
point.

For `INSERT` and `UPDATE`, a recognised conflict is a contextual
`NotCommitted` result. No rejected row or field change remains. Updating one
object with its current unique Text succeeds. Two concurrent transactions
that claim the same byte-exact Text value produce one committed change and one
typed conflict. PostgreSQL chooses the winner.

U+0000 remains unavailable under the existing raw Text rules and cannot become
a stored uniqueness value through those paths.

## Protected raw behaviour

Existing raw Text `INSERT` and selector/value `UPDATE` calls need no command,
ORV1, ORF1, socket, or adapter change. Authorisation and the execute audit
continue to occur before active target, parameter, type, value, or constraint
inspection. A denied call returns `EXECUTE_DENIED` and discloses no schema or
value fact.

An allowed raw call that reaches `UniqueTextConflict` rolls back its mutation
savepoint. The outer protected transaction commits the allowed execute audit
and returns the existing protocol-version-one `INTERNAL_FAILURE`. The public
frame, audit record, and log do not contain the Text value or a private
PostgreSQL identifier. Target-shape failures remain `TARGET_UNAVAILABLE`.
Database, audit, savepoint, outer commit, driver, shutdown, and unknown-outcome
failures remain internal under their existing rules.

## Replay and restart

Exact source replay retains the field identity, unique fact, private physical
identities, rows, functions, parameters, and grants. It creates no second
constraint and needs no regrant. A semantic field rename retains the same
`FieldId` and therefore the same private constraint and equality rule.

Restart recovery verifies the exact Text type, nullability, collation,
constraint, and index before it exposes the active revision. Public replay and
restart reads prove that stored rows remain. Recovery retains the original
function and parameter identities and every stored execute grant.

## Required proof

Compiler and core proof must establish:

* acceptance of nullable and required Text uniqueness through the exact
  version-one scalar and verified version-two value authorities;
* acceptance of existing required unique References without change;
* rejection of nullable References, every other scalar or value contract,
  named, enum, record, constructed, collection, opaque, and `VOID` fields with
  the exact diagnostic and source span;
* hostile checked input cannot bypass the same accepted-shape rule during
  preparation;
* backend-neutral physical projection retains the Text type, nullability, and
  unique fact; and
* exact replay is unchanged while every existing-object uniqueness or
  collation change remains closed.

Focused PostgreSQL proof must establish:

* exact `text COLLATE pg_catalog."C"` lowering for nullable and required Text,
  stable `uq_<FieldId>` naming, and the complete one-column immediate index
  shape;
* verification rejects missing, renamed, crossed, default or wrong collation,
  non-deterministic collation, non-immediate, deferrable, partial, expression,
  included, multi-column, invalid, unready, and `NULLS NOT DISTINCT` variants;
* byte-identical Text conflicts while case variants, empty Text, whitespace,
  line-ending variants, and canonically equivalent but byte-distinct Unicode
  values remain independent;
* multiple nullable `NULL` values store successfully;
* duplicate `INSERT` and `UPDATE` return the exact typed conflict, preserve
  prior rows, and expose no attempted Text;
* assigning an object's existing unique Text to itself succeeds;
* two concurrent claims produce exactly one commit and one typed conflict
  without a preflight query or partial state;
* an unrelated `23505` remains generic; and
* apply, recovery, replay, semantic rename, hostile `search_path`, transaction
  cleanup, and connection cleanup preserve the exact contract.

Focused server proof must use the authenticated local socket. It must establish
denial before target facts, one successful Text insert, duplicate insert and
selector/value update redacted as `INTERNAL_FAILURE`, unchanged rows, retained
allowed audits, flow-control and cancellation retention, and the private typed
source without adapter-side catalogue inspection.

The installed proof uses only the reproduced package, checked-in sources,
public `/usr/bin/orna` commands, the installed service account, and the local
raw socket. It must create required and nullable unique Text rows, store more
than one nullable `NULL`, reject duplicate non-null Text without changing
rows, preserve byte-distinct values, update one selected object, replay without
regrant, rename the field and retain its rows and duplicate rejection, restart,
and reuse the original function and parameter identities and grants. It must
not claim or inspect private field, column, constraint, or index identities.
Focused core and PostgreSQL proof owns those private identity claims.

Every test line, helper, and fixture remains owned by the approved test
implementation session. Production implementation, architecture, and hard
debugging remain owned by the host GPT-5.6 model. Format, strict Clippy,
rustdoc, diff, similarity, workspace, live PostgreSQL, socket, installed
package, replay, rename, restart, security-audit, concurrency, and session-
cleanup gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. A focused RED behaviour
tracer precedes the smallest production change that makes it green.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(schema): define unique Text fields` | this ADR; `docs/decisions/README.md` | Accept and index the exact type, equality, null, physical, conflict, security, replay, migration, closure, and proof contract. |
| `feat(compiler): admit unique Text fields` | `crates/orna-compiler/src/lib.rs`; `crates/orna-compiler/src/resolver.rs`; `crates/orna-compiler/src/prepare.rs` | Add the DeepSeek-owned version-one, verified version-two, acceptance, and rejection tracers, then admit only the exact Text and retained required-Reference source shapes. |
| `feat(core): plan unique Text fields` | `crates/orna-core/src/physical.rs` | Add the DeepSeek-owned physical matrix, then retain unique Text only after exact versioned projection. |
| `test(postgres): trace unique Text fields` | `crates/orna-postgres/tests/apply.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Add the focused ignored live RED physical, recovery, conflict, concurrency, rollback, classification, and replay tracers. |
| `feat(postgres): enforce unique Text fields` | `crates/orna-postgres/src/kernel/physical.rs`; `crates/orna-postgres/src/kernel/physical/verify.rs`; `crates/orna-postgres/src/kernel/server_mutation_execution.rs` | Lower and verify exact Text collation and classify exact Text conflicts without changing Reference conflicts. DeepSeek owns every test line in these files. |
| `test(server): prove unique Text authority` | `crates/orna-server/tests/standard_database.rs` | Prove authenticated socket success, denial, conflict redaction, typed private source, audit, flow control, cancellation, and cleanup. |
| `test(system): exercise installed unique Text fields` | `crates/orna-system-tests/fixtures/product_test_unique_text.orna`; `crates/orna-system-tests/fixtures/product_test_unique_text_renamed.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove public required and nullable uniqueness, byte-distinct values, conflict rollback, update, replay, rename behaviour, restart, function and parameter identities, and grants. |

## Deferred surface

This decision does not accept uniqueness for Boolean, Integer, BigInt, Float,
Bytes, Decimal, UUID, date-time, Duration, enum, record, another named value,
constructed, collection, opaque, or `VOID` fields. It does not accept nullable
unique References.

It also defers composite, cross-field, cross-type, cross-database,
case-insensitive, normalised, locale-selected, partial, conditional,
expression, included, deferrable, or `NULLS NOT DISTINCT` uniqueness. It adds
no conflict clause, upsert, preflight query, retry, caller-selected physical
name, caller-selected collation, existing-object uniqueness migration, remote
transport, ORV2 through ORV5, ORF2 through ORF5, or `sys.invoke` path.

## Precedence

For nullable and required unique Text fields on newly created object types,
this decision supersedes work ADR 0005's blanket unique-field deferral and
work ADR 0013's scalar and nullable uniqueness closures. It preserves work ADR
0013's complete required unique Reference contract and every other rejected
Reference shape.

It preserves work ADRs 0004, 0006, 0007, 0008, 0024, 0025, 0026, 0043, 0045,
0046, 0049, and 0050 except that their existing physical, `INSERT`, and
`UPDATE` paths may now encounter the exact private Text constraint and typed
conflict defined here. Their source identity, mutation artifact, command,
codec, framing, transport, authorisation, audit, savepoint, cancellation,
result, recovery, and public redaction contracts do not change.

This decision narrows the broader unique Text examples in
`spec/docs/03-quick-tour.md`, `spec/docs/12-object-relational-model.md`, and
`spec/docs/22-ddl-reference.md`. For the exact scope above, this accepted
record has precedence. It changes no canonical specification file and does not
advance or weaken the constructed-value and `sys.invoke` sequence in work ADRs
0036, 0039, or 0042.
