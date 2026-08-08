# ADR 0006: Field Renames Are Replay-Safe Identity Transitions

**Status:** Accepted

## Decision

The first supported semantic rename is:

```sql
ALTER TYPE people.person
    RENAME FIELD email TO primary_email;
```

This operation preserves the field's stable `FieldId`. It does not create a
new field, retain an alias for the old name, or infer continuity from field
order or shape.

An apply bundle remains one complete final candidate source snapshot. The
snapshot must contain the final object declaration and final dependent source:

```sql
CREATE TYPE people.person AS OBJECT (
    primary_email TEXT NOT NULL
);

ALTER TYPE people.person
    RENAME FIELD email TO primary_email;

CREATE SERVER FUNCTION people.list_emails()
RETURNS ROWS (email TEXT)
AS
    SELECT person.primary_email
    FROM people.person person;
```

The `ALTER TYPE` statement is transition evidence between the expected base
catalogue and the final declarations in the candidate. It is not an
imperative statement applied in source order. Its source-unit position and its
position before or after the final declaration do not change its meaning.

The candidate source revision stores the exact source, including the rename
statement.

## Identity binding

The target object type must have the same exact semantic name in the expected
base catalogue and the complete candidate. The candidate must declare the new
field name and must not declare the old field name.

After identifier normalisation, the compiler handles the four possible base
states as follows:

| Old name in base | New name in base | Result |
| --- | --- | --- |
| present | absent | Bind the old field's `FieldId` to the final new declaration. |
| absent | present | Treat the transition as already satisfied and bind the new field's `FieldId`. |
| present | present | Reject the rename as a name collision. |
| absent | absent | Reject the rename because the old field does not exist. |

The already-satisfied case makes an exact active source snapshot safe to submit
again against the active revision that the snapshot produced. This is
state-based idempotence.
The active catalogue does not prove that the historical old spelling existed,
so the compiler does not claim that it authenticated a prior rename. A later
snapshot can remove the rename statement. Exact-name resolution of the new
declaration then preserves the same `FieldId`.

The compiler rejects:

* an old and new name that normalise to the same semantic name;
* a missing base or candidate object type;
* a candidate that does not contain the final new declaration;
* a candidate that still contains the old declaration;
* more than one rename that consumes the same old field;
* more than one rename that produces the same new field;
* a new name already owned by a different field identity;
* rename chains and swaps in one candidate.

The compiler does not infer a rename from a matching ordinal, type, default,
nullability, or any other field property. Without explicit transition
evidence, removing one field name and adding another is a drop and an add with
different identities. Unsupported drops and physical changes continue to fail
closed.

## Semantic graph and source origins

The final catalogue stores the new semantic field name with the original
`FieldId`.

The `DefinitionOrigin` for that field points to the final field declaration in
the candidate `CREATE TYPE` statement. It does not point to the `ALTER TYPE`
statement. The rename statement is transition evidence, not a definition.
On a first application from the old-present and new-absent state, the parent
source revision retains the old declaration. An already-satisfied application
makes no claim that any retained source revision contains the old spelling.

Each dependent definition must use the new field name in its final source.
Resolution records the same owner-qualified `FieldId` as before the rename.
The `DefinitionReference` source origin points to the exact new field-name
token in the dependent candidate source.

A rename-only source change does not change a dependent function's resolved
plan or semantic reference targets. The compiler therefore reuses the
function's immutable revision and executable artefact. It does not allocate a
new function revision. The active `DefinitionOrigin` for the function points
to its final candidate declaration, while the reused immutable revision keeps
its original declaration record.

The source-bundle and catalogue hashes change. The catalogue hash includes the
new semantic name, current definition origins, and current reference origins.
The function semantic hash does not change because semantic names and source
origins are not executable semantics.

## Physical storage

This field rename produces no PostgreSQL DDL. The private column remains
`f_<field-id-hex>` because physical names use stable Orna identities rather
than semantic names.

Physical planning compares the storage projection of an existing object. It
ignores object-type and field semantic names. It continues to compare the
object `TypeId`, field count and order, `FieldId`, ordinal, resolved type,
nullability, uniqueness, default-expression identity, and delete action. A
change to any compared property remains an unsupported existing-object change.

## Apply and recovery

Compiler checking and preparation validate the rename transition against the
expected base catalogue before apply. Apply receives the prepared candidate,
uses the expected base revision, and remains atomic. In one PostgreSQL
transaction it:

1. locks the active revision and rejects a stale expected base;
2. validates the complete prepared candidate and its hashes;
3. confirms that the physical plan contains no change for the rename;
4. persists the exact complete source snapshot, semantic catalogue, origins,
   references, hashes, and reused function revision link;
5. advances the active source and catalogue pair;
6. recovers the candidate and verifies its hashes and physical storage.

Any failure leaves the prior active revision and physical storage unchanged.
After restart, recovery reconstructs the exact source containing the rename
statement, the new semantic field name, the original `FieldId`, the stable
dependent references, and the unchanged private column.

## Deferred surface

This decision does not accept:

* `ALTER TYPE ... RENAME TO ...`;
* `ALTER FUNCTION ... RENAME TO ...`;
* field moves between object types;
* rename chains or swaps in one candidate;
* an old-name compatibility alias;
* a combined rename and physical field change.

Type and function renames require separate accepted identity and dependency
rules. Function rename support must wait until the compiler can resolve and
test dependent function-call references.

## Precedence

This record amends ADR 0004. ADR 0004 states that the first kernel rejects
unimplemented renames. The exact replay-safe field rename defined here is now
a supported semantic change and is not a physical migration. The rejection in
ADR 0004 continues to apply to type renames, function renames, and field
renames outside this decision's exact contract.

This record also makes the field-rename direction in
`spec/docs/22-ddl-reference.md` concrete. For this subject, this accepted
decision has precedence over current proposals and derived examples.
