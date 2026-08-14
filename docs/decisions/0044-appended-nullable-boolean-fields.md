# ADR 0044: Existing Objects Admit One Appended Nullable Boolean Field

**Status:** Accepted

## Decision

One source apply may add one nullable Boolean field to one existing durable
object type. The complete candidate source declares the final object shape:

```sql
CREATE TYPE product_test.probe AS OBJECT (
    stored BOOLEAN NOT NULL,
    added BOOLEAN
);
```

There is no `ALTER TYPE ... ADD FIELD` statement. The candidate declaration is
the definition; comparison with the recovered active catalogue determines that
`added` is new. Preparation allocates one new stable `FieldId`. Exact replay
resolves that name to the now-active field identity and plans no physical
change.

This is a deliberately narrow first existing-object storage transition. It
does not infer a rename, reuse a removed identity, or treat field position as
identity evidence.

## Admitted transition

The transition is accepted only when all of these facts hold:

* the active and candidate object have the same stable `TypeId`;
* every active field remains, in exact ordinal order, as an unchanged physical
  prefix of the candidate fields;
* the candidate has exactly one additional field after that prefix;
* no other existing object has a physical change in the same candidate;
* the added field is a standard `BOOLEAN`, is nullable, is not `UNIQUE`, has no
  default expression, and has no delete action; and
* the candidate remains valid under every existing catalogue, source,
  reference, hash, and executable-artifact rule.

New object creation may remain in the same candidate because it already has an
independent accepted physical plan. The one-field limit applies only to
changes of existing objects.

Changing an existing field's identity, ordinal, resolved type, nullability,
uniqueness, default, or delete action remains
`PhysicalPlanError::UnsupportedExistingObjectChange`. Removing a field or
object, inserting a new field before or between active fields, adding a
required field, adding two fields, or changing two existing objects also
remains closed. Enum, record, reference, other scalar, constructed, defaulted,
and unique field additions are deferred.

## Backend-neutral plan

`PhysicalPlan` retains complete new-object creation and adds one optional
existing-object field operation. The operation carries only the stable owner
`TypeId` and the normal projected `CreateField`. It contains no PostgreSQL
relation name, column name, SQL, source spelling, or runtime value.

Planning first projects the complete active and candidate objects through the
existing physical type authority. Exact projections still produce no change.
For one unequal existing object, planning requires the exact active prefix and
the admitted final field. It returns no partial plan on failure.

The new public plan view is evidence for storage adapters and tests. It is not
a general migration language and does not expose it through the installed CLI.

## PostgreSQL installation and recovery

The PostgreSQL adapter lowers the admitted operation to one `ALTER TABLE` that
adds the private identity-derived Boolean column. The private table and column
names continue to be derived only from `TypeId` and `FieldId`. The column has
no default and accepts SQL `NULL`.

The adapter establishes the same exact trusted `pg_catalog` search path before
the statement. In a mixed plan it executes every new relation creation and
revocation in candidate order, then the single `ADD COLUMN`, then every
new-object reference constraint in candidate order. This preserves the
existing pre-reference phase: every required relation and column exists before
any reference constraint is installed. Semantic catalogue persistence and the
active revision pointer follow those physical statements. PostgreSQL
transactional DDL keeps the physical addition in the existing apply
transaction.

Rows that existed before the transition receive SQL `NULL` for the new column.
No backfill, scan, application callback, trigger, generated expression, or
default is executed. Later normal INSERTs may omit the field and store `NULL`,
or may explicitly store a Boolean through the already accepted mutation path.

After the active pointer moves, normal recovery reconstructs the candidate and
the existing physical verifier requires the exact appended private column,
type, nullability, order, constraints, and access controls. Apply requests
commit only after that recovery succeeds. Every failure known to occur before
a successful commit leaves the prior active pair and prior physical table
unchanged. A commit with an unknown outcome, or a driver or shutdown failure
after commit may have succeeded, fails closed without claiming rollback or
automatic retry. This retains work ADR 0038's existing commit-outcome
boundary.

## Source, function, and authority continuity

The complete source snapshot and catalogue hash change. The existing object's
`TypeId` and every retained field's `FieldId` remain stable. Unchanged
functions retain their `FunctionId`; normal semantic hashing decides whether
their immutable revision and artifact are reused.

Stored EXECUTE grants continue to name stable `FunctionId` values and remain
effective. Newly declared functions receive no implicit grant. Source apply
output continues to report the complete sorted function and parameter
discovery document from work ADR 0038; it does not expose field identities or
physical migration facts.

## Required proof

Core behavioural proof must establish:

* one exact appended nullable Boolean field produces one owner-and-field plan;
* semantic object and field names do not affect the physical prefix decision;
* exact replay produces no field operation;
* a new object may be created in the same plan;
* required, defaulted, unique, non-Boolean, reordered, inserted, removed,
  changed, second-field, and second-object transitions remain exact typed
  failures; and
* a candidate containing multiple invalid existing-object changes returns only
  an error and never exposes a partial plan. This decision does not add a new
  observable rule for choosing one error among several independently invalid
  changes.

PostgreSQL proof must use a real disposable repository-local database and the
normal kernel apply path. It must establish that a live row survives the
transition with SQL `NULL`, an omitted post-transition value is also `NULL`,
an explicit Boolean value is stored, the candidate is recovered in the same
transaction and through a new kernel connection, and every kernel table plus
physical row remains unchanged after an induced failed apply. Exact private
SQL lowering may have a focused unit proof, but source-text containment is not
the delivery proof.

The installed proof uses only the exact reproduced package, checked-in source,
public `/usr/bin/orna` commands, the installed service account, and the public
raw socket. It must:

* apply the existing one-field source, grant its existing creator and reader,
  and create one live row;
* apply one complete source that appends `added BOOLEAN` and adds a creator
  that stores an explicit Boolean plus a reader for the new field;
* prove the existing function identities, grants, object reference target, and
  old stored value survive;
* prove the new functions are denied before explicit grants;
* read the pre-transition row as one typed Boolean `NULL`;
* call the unchanged old creator after the transition and read another typed
  `NULL` for its omitted field;
* call the new creator and read its explicit Boolean value;
* replay the exact expanded source without regrant and retain the complete
  function discovery and values; and
* restart the installed service and retain the unordered new-field multiset
  and every callable grant.

Every test line and fixture remains owned by the approved DeepSeek test
session. Production implementation, architecture, and difficult debugging
remain owned by the host GPT-5.6 model. Installed-package, strict Clippy,
rustdoc, format, diff, similarity, workspace, and live PostgreSQL gates remain
required.

## Implementation sequence

Each row is one signed Conventional Commit, changes one to three files, and
leaves the repository buildable and green.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(storage): define appended nullable Boolean fields` | this ADR; `docs/decisions/README.md` | Accept and index the exact semantic, physical, transaction, recovery, continuity, closure, and proof contract. |
| `feat(core): plan one appended nullable Boolean field` | `crates/orna-core/src/physical.rs` | Add the backend-neutral operation and complete public behavioural matrix. DeepSeek owns every test line. |
| `feat(postgres): install one appended nullable Boolean field` | `crates/orna-postgres/src/kernel/physical.rs`; `crates/orna-postgres/tests/apply.rs` | Lower the operation and prove normal live apply, data preservation, same-transaction recovery, restart recovery, and rollback. DeepSeek owns every test line. |
| `test(system): exercise installed nullable field addition` | `crates/orna-system-tests/fixtures/product_test_added_nullable.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove the complete packaged live-data, NULL, grant, replay, and restart journey. DeepSeek owns both files' test logic. |

## Deferred surface

This decision does not admit field removal, object removal, field reordering,
more than one existing-object field addition per apply, another physical
existing-object change, required/defaulted/unique additions, another scalar,
enum, record, reference, constructed, collection, or opaque field addition,
data backfill, a user-written migration, online index construction, concurrent
schema migration, downgrade, or automatic retry.

It does not add a command, SQL endpoint, external PostgreSQL connection,
configuration option, ORV5/ORF5 behaviour, sealed invocation carrier, or
`sys.invoke` path.

## Precedence

This decision supersedes the existing-object field-addition closure in work
ADRs 0003, 0004, 0006, 0017, 0038, and the initial physical planner for only the
exact appended nullable Boolean scope above. It preserves all other source,
identity, rename, physical creation, apply, recovery, package, security,
protocol, and failure contracts.
