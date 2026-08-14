# ADR 0046: Existing Objects Admit One Appended Nullable Executable Scalar Field

**Status:** Accepted

## Decision

One source apply may add one nullable executable scalar field to one existing
durable object type. This decision admits exactly these six standard scalar
representations:

* `BOOLEAN`;
* `INTEGER`;
* `BIGINT`;
* `FLOAT`;
* `CHARACTER LARGE OBJECT`, with `TEXT` as its prelude alias; and
* `BINARY LARGE OBJECT`, with `BYTES` as its prelude alias.

For example, the complete candidate source can declare this final object
shape:

```sql
CREATE TYPE product_test.probe AS OBJECT (
    stored BOOLEAN NOT NULL,
    added INTEGER
);
```

There is no `ALTER TYPE ... ADD FIELD` statement. The candidate declaration is
the complete definition. Comparison with the recovered active catalogue
determines that `added` is new. Preparation allocates one new stable `FieldId`.
Exact replay resolves that name to the now-active field identity and plans no
physical change.

This decision generalises only the field-type admission from work ADR 0044.
It does not infer a rename, reuse a removed identity, treat field position as
identity evidence, or admit a second existing-object storage change.

## Exact resolved-type admission

The candidate `FieldDefinition::resolved_type()` must be exactly
`ResolvedType::Value(value_type)`. The candidate's pinned standard catalogue
must contain that `TypeId` as a `PRIMITIVE`, `IMMUTABLE`, and `PERSISTABLE`
value definition with exactly one of these kernel contracts:

| Kernel contract | Backend-neutral projection | PostgreSQL storage |
| --- | --- | --- |
| `orna.kernel.value.boolean@1` | `PhysicalFieldType::Scalar(StandardScalar::Boolean)` | `boolean` |
| `orna.kernel.value.integer@1` | `PhysicalFieldType::Scalar(StandardScalar::Integer)` | `integer` |
| `orna.kernel.value.bigint@1` | `PhysicalFieldType::Scalar(StandardScalar::BigInt)` | `bigint` |
| `orna.kernel.value.float@1` | `PhysicalFieldType::Scalar(StandardScalar::Float)` | `double precision` |
| `orna.kernel.value.character-large-object@1` | `PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)` | `text` |
| `orna.kernel.value.binary-large-object@1` | `PhysicalFieldType::Scalar(StandardScalar::BinaryLargeObject)` | `bytea` |

The admission rule uses the resolved value identity and its pinned definition.
It does not compare source spelling, reconstruct a `TypeId` from a scalar, or
admit every value type that currently has a PostgreSQL storage mapping.
`ResolvedType::Scalar`, including a legacy occurrence of one of the six
scalars, is not a deployable-candidate admission path.
`ResolvedType::Named` and `ResolvedType::Reference` also remain closed.

`BOOLEAN` and `BOOL`, `INTEGER` and `INT`, `CHARACTER LARGE OBJECT` and `TEXT`,
and `BINARY LARGE OBJECT` and `BYTES` resolve through their existing canonical
standard bindings. Their spelling does not create separate physical types or
separate transition rules.

## Admitted transition

The transition is accepted only when all of these facts hold:

* the active and candidate object have the same stable `TypeId`;
* every active field remains, in exact ordinal order, as an unchanged physical
  prefix of the candidate fields;
* the candidate has exactly one additional field after that prefix;
* no other existing object has a physical change in the same candidate;
* the added field has one exact admitted resolved type, is nullable, is not
  `UNIQUE`, has no default expression, and has no delete action; and
* the candidate remains valid under every existing catalogue, source,
  reference, hash, and executable-artifact rule.

New object creation may remain in the same candidate because it has an
independent accepted physical plan. The one-object and one-field limits apply
only to changes of existing objects.

Changing an existing field's identity, ordinal, resolved type, nullability,
uniqueness, default, or delete action remains
`PhysicalPlanError::UnsupportedExistingObjectChange`. Removing a field or
object, inserting a new field before or between active fields, adding a
required field, adding two fields, or changing two existing objects also
remains closed. `DECIMAL`, `UUID`, `DATE`, `TIME`, `TIMESTAMP`, `DURATION`,
`VOID`, enum, record, reference, constructed, collection, and opaque field
additions remain closed.

## Backend-neutral plan

`PhysicalPlan` retains complete new-object creation and its one optional
existing-object field operation. The operation carries only the stable owner
`TypeId` and the normal projected `CreateField`. It contains no PostgreSQL
relation name, column name, SQL, source spelling, standard-library constant,
or runtime value.

Planning first projects the complete active and candidate objects through the
existing physical type authority. Exact projections still produce no change.
For one unequal existing object, planning requires the exact active prefix and
the one admitted final field. The admission predicate accepts only the six
backend-neutral scalar projections listed above and retains the exact resolved
value-type gate before projection. It returns no partial plan on failure.

The public plan view remains evidence for storage adapters and tests. It is not
a general migration language and does not expose field addition through the
installed command-line interface.

## PostgreSQL installation and recovery

The PostgreSQL adapter already lowers all six admitted projections for new
object creation. It uses the same mapping for one `ALTER TABLE` statement that
adds the private identity-derived column to an existing object. The private
table and column names continue to be derived only from `TypeId` and `FieldId`.
The new column has no default and accepts SQL `NULL`.

The adapter establishes the same exact trusted `pg_catalog` search path before
the statement. In a mixed plan it executes every new relation creation and
revocation in candidate order, then the single `ADD COLUMN`, then every
new-object reference constraint in candidate order. This preserves the
existing pre-reference phase. Every required relation and column exists before
any reference constraint is installed. Semantic catalogue persistence and the
active revision pointer follow those physical statements. PostgreSQL
transactional data definition keeps the physical addition in the existing
apply transaction.

Rows that existed before the transition receive SQL `NULL` for the new column,
including variable-width `text` and `bytea` columns. No backfill, table scan,
application callback, trigger, generated expression, or default is executed.
Later normal INSERTs may omit the field and store `NULL`, or may explicitly
store a value through an already accepted mutation path for that scalar. Work
ADR 0045 remains the raw scalar argument and value-preservation authority. This
decision adds no invocation or mutation behaviour.

After the active pointer moves, normal recovery reconstructs the candidate.
The existing physical verifier requires the exact appended private column,
type, nullability, order, constraints, and access controls. Apply requests
commit only after that recovery succeeds. Every failure known to occur before
a successful commit leaves the prior active pair and prior physical table
unchanged. A commit with an unknown outcome, or a driver or shutdown failure
after commit, may have succeeded and fails closed without claiming rollback or
automatic retry. This preserves work ADR 0038's commit-outcome boundary.

## Source, function, grant, and replay continuity

The complete source snapshot and catalogue hash change. The existing object's
`TypeId` and every retained field's `FieldId` remain stable. Unchanged functions
retain their `FunctionId`. Normal semantic hashing decides whether their
immutable revision and executable artifact are reused.

Stored `EXECUTE` grants continue to name stable `FunctionId` values and remain
effective. Newly declared functions receive no implicit grant. Source apply
output continues to report the complete sorted function and parameter
discovery document from work ADR 0038. It does not expose field identities or
physical migration facts.

Exact replay of the expanded source creates no physical field operation. It
does not require a new grant and does not change stored rows. Restart recovery
retains the same active pair, stable identities, grants, executable artifacts,
and typed values.

## Required proof

Core behavioural proof must establish:

* an exact success matrix for `BOOLEAN`, `INTEGER`, `BIGINT`, `FLOAT`,
  `CHARACTER LARGE OBJECT`, and `BINARY LARGE OBJECT`;
* exact `ResolvedType::Value` admission through each pinned primitive,
  immutable, persistable kernel contract, with no spelling or fixed `TypeId`
  reconstruction;
* rejection of legacy `ResolvedType::Scalar`, unsupported or missing value
  contracts, `DECIMAL`, `UUID`, date-time types, `DURATION`, `VOID`, enum,
  record, reference, and constructed types;
* semantic object and field names do not affect the physical prefix decision;
* exact replay produces no field operation;
* a new object may be created in the same plan;
* required, defaulted, unique, delete-action, reordered, inserted, removed,
  changed, second-field, and second-object transitions remain exact typed
  failures; and
* a candidate containing multiple invalid existing-object changes returns only
  an error and never exposes a partial plan. This decision adds no observable
  rule for choosing one error among independently invalid changes.

Focused PostgreSQL proof must establish the exact six-entry lowering matrix.
Real disposable repository-local database proof must use the normal kernel
apply path and cover each of the six admitted types. For each type, one live row
must survive with SQL `NULL`, an omitted post-transition value must also be
`NULL`, and an explicit value must round-trip with its exact runtime type. The
proof must also cover same-transaction recovery, recovery through a new kernel
connection, and an induced failed apply that leaves every kernel table and
physical row unchanged. Source-text containment is not delivery proof.

The installed proof uses only the exact reproduced package, checked-in source,
public `/usr/bin/orna` commands, the installed service account, and the public
raw socket. It must retain the work ADR 0044 Boolean journey and add at least
these two non-Boolean representatives:

* `INTEGER` as a fixed-width scalar; and
* `TEXT` as a variable-width scalar.

For both representatives, the installed proof must:

* apply an existing source, grant its existing creator and reader, and create
  one live row before the field exists;
* apply one complete source that appends only that nullable field and declares
  one exact scalar-parameter creator plus a reader for the new field;
* prove the existing function identities, grants, object reference target, and
  old stored value survive;
* prove the new functions are denied before explicit grants;
* read the pre-transition row as a typed `NULL` of the exact new scalar;
* call the unchanged old creator after the transition and read another typed
  `NULL` for its omitted field;
* call the new creator with one canonical work ADR 0045 ORV1 value and read its
  exact explicit value;
* replay the exact expanded source without regrant and retain complete function
  discovery and values; and
* restart the installed service and retain the unordered new-field values and
  every callable grant.

Every test line and fixture remains owned by the approved DeepSeek test
session. Production implementation, architecture, and difficult debugging
remain owned by the host GPT-5.6 model. Installed-package, strict Clippy,
rustdoc, format, diff, similarity, workspace, and live PostgreSQL gates remain
required.

## Implementation sequence

Each row is one signed Conventional Commit, changes one to three files, and
leaves the repository buildable and green. DeepSeek owns every test and fixture
line named in this sequence.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(storage): define appended nullable scalar fields` | this ADR; `docs/decisions/README.md` | Accept and index the exact resolved-type, physical, transaction, recovery, grant, replay, closure, and proof contract. |
| `feat(core): admit one appended nullable scalar field` | `crates/orna-core/src/physical.rs` | Generalise only the admission predicate and add the complete DeepSeek-owned success and rejection matrix. |
| `test(postgres): prove appended nullable scalar fields` | `crates/orna-postgres/tests/apply.rs` | Prove through the existing six scalar lowerings that live apply preserves data, installs exact typed values, recovers, and rolls back. DeepSeek owns every changed line. No PostgreSQL production change is manufactured. |
| `test(system): exercise installed nullable scalar additions` | `crates/orna-system-tests/fixtures/product_test_added_nullable_integer.orna`; `crates/orna-system-tests/fixtures/product_test_added_nullable_text.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Add the DeepSeek-owned installed `INTEGER` and `TEXT` journeys, including live rows, typed `NULL`, explicit values, grants, replay, and restart. |

## Deferred surface

This decision does not admit field removal, object removal, field reordering,
more than one existing-object field addition per apply, a change to more than
one existing object, another physical existing-object change,
required/defaulted/unique additions, or any delete action on the added field.

It defers `DECIMAL`, `UUID`, `DATE`, `TIME`, `TIMESTAMP`, `DURATION`, `VOID`,
enum, record, reference, constructed, collection, and opaque field additions.
It also defers data backfill, a user-written migration, online index
construction, concurrent schema migration, downgrade, and automatic retry.

It does not add a command, SQL endpoint, external PostgreSQL connection,
configuration option, runtime value codec, sealed invocation carrier, or
`sys.invoke` path.

## Precedence

This decision supersedes only work ADR 0044's Boolean-only admission and its
closure of other executable scalar additions. It preserves work ADR 0044's
one-object, one-field, exact-prefix, nullable, no-default, no-unique,
no-delete-action, transaction, recovery, identity, grant, replay, failure, and
proof contracts.

For the exact six-scalar scope above, this decision also supersedes the
existing-object field-addition closure in work ADRs 0003, 0004, 0006, 0017,
0038, and the initial physical planner. It preserves all other source,
identity, rename, physical creation, apply, recovery, package, security,
protocol, and failure contracts.
