# 7. Tables, rows and assertions {#tables}

## One persistent relational noun

**ORNA-TABLE-001** `table` MUST be the only core declaration for persistent relational data.

The following declaration kinds are not part of conforming version 1.0 syntax:

```text
log
view
store
ingest
source
on
```

## Explicit primary keys

```orna
pub table Contact(id: Str) {
    name: Str,
    emails: [Str],
}
```

Parameters before the body form the ordered primary key. Body fields are non-key columns.

**ORNA-KEY-001** A table parameter list MUST define the complete primary key in parameter order.

**ORNA-KEY-002** Primary-key fields MUST be visible as ordinary logical row fields even when a virtual/interchange row document obtains its key from a schema-aware path. Canonical persisted rows retain the complete key in the primary index.

## Composite keys

```orna
pub table Reading(sensor: Uuid, time: Instant) {
    temperature: Decimal,
    humidity: Decimal,
}
```

The logical key is `(sensor, time)`.

## Automatic keys

```orna
pub table Note {
    created: Instant,
    text: Str,
}
```

**ORNA-AUTOID-001** A table without explicit key parameters MUST receive an implicit monotonically increasing `Int` key named `id`.

**ORNA-AUTOID-002** IDs allocated through one repository instance and its current hidden allocator MUST never be reused after deletion, reset, checkout or branch switching.

**ORNA-AUTOID-003** Gaps caused by failed or abandoned allocations are valid and expected.

**ORNA-AUTOID-004** The current allocator high-water mark MUST be stored outside rewindable branch trees using a hidden Git ref or an equivalent atomic monotonic mechanism.

Suggested ref:

```text
refs/orna/ids/<table-object-id>
```

**ORNA-AUTOID-005** A committed snapshot MUST include the allocator watermark known at that snapshot for historical introspection.

**ORNA-AUTOID-006** Independent offline clones have independent allocators and MAY allocate the same integer. A merge of different rows with the same automatic ID MUST create a typed row conflict and MUST NOT silently renumber either row or its references.

Where collision-free independent creation is required, the schema declares an explicit distributed key:

```orna
pub table Event(id: Uuid = uuid7()) {
    time: Instant,
    value: Str,
}
```

## Key defaults

```orna
pub table Contact(
    id: Uuid = uuid7()
) {
    name: Str,
    emails: [Str],
}
```

**ORNA-KEYDEF-001** A key default MUST be an ordinary Orna expression evaluated once inside the insertion transaction.

**ORNA-KEYDEF-002** A supplied key MUST bypass its default expression.

**ORNA-KEYDEF-003** A generated key MUST be stored as the row's identity and MUST NOT be recomputed when referenced fields later change.

**ORNA-KEYDEF-004** Key defaults that inspect current table state are allocation logic, not deterministic pure identities. Independent branches MAY allocate colliding or different keys; semantic merge MUST report resulting conflicts honestly.

## Stored, defaulted and computed fields

A table body has three field lifecycles:

```orna
pub table Contact(id: Str) {
    first: Str,
    last: Str,
    country: Str = "GB",
    full_name: Str => "{first} {last}",
}
```

| Form | Name | Logical behavior |
|---|---|---|
| `field: T` | required stored field | caller supplies a value and the row stores it |
| `field: T = expression` | defaulted stored field | expression runs once when an insertion omits the field; the resulting value belongs to that row |
| `field: T => expression` | computed field selector | no stored logical value; expression is evaluated from the row when observed |

**ORNA-FIELD-001** An insertion-time default is an ordinary expression evaluated once inside the insertion transaction. A supplied value bypasses it.

**ORNA-FIELD-002** The logical value selected for a defaulted field MUST NOT be recomputed merely because the declaration, table contents or referenced names later change.

**ORNA-FIELD-003** When a closed deterministic constant default is introduced on a table containing older rows, snapshot metadata MAY record one frozen fallback value for physically missing fields. That frozen value belongs to the field-introduction revision. Later edits to the default affect future insertions only.

**ORNA-FIELD-004** Future insertions logically store the evaluated default result. A storage profile MAY omit a value physically when it can reconstruct the row's already-frozen logical value without consulting a later declaration.

**ORNA-FIELD-005** A row-dependent default may be used for future insertions, but introducing it to existing rows requires an explicit backfill or an optional field. Orna MUST NOT create a hidden compute-on-read era for a stored field.

**ORNA-FIELD-006** `=>` declares a computed field. It is not stored as a logical row value, cannot be supplied during `insert`, and cannot be changed through `update`.

**ORNA-FIELD-007** A computed field is evaluated from the row under the current snapshot whenever observed and automatically reflects changes to its stored-field dependencies.

**ORNA-FIELD-008** A computed expression MUST be deterministic and row-local. It may read the current row and call pure helpers; it MUST NOT mutate tables, query unrelated tables, open streams, access secrets, perform I/O, or use ambient current time/randomness.

**ORNA-FIELD-009** Within a key-default or computed-field expression, each table field name is available as an immutable lexical selector of the prospective/current row, and `self` names the complete row context. A local binding may shadow a field only inside an explicitly nested block.

**ORNA-FIELD-010** Every stored/defaulted/computed field creates a selector such as `Contact.full_name`; `contact.full_name` applies it. Field dependencies appear in `sys.Dependency`.

**ORNA-FIELD-011** The planner MAY cache/materialize a computed selector, but this MUST NOT change its value, mutation rules, dependencies or serialization. Computed fields are excluded from standalone stored-row encoding unless a codec explicitly requests a derived projection.

The difference between a computed field and an ordinary function is fieldhood, not computation: declaration inside the table gives parameterless property access and row-local restrictions. Arbitrary work remains an explicitly called or piped function.

## Table-owned assertions

A table declaration owns propositions about its complete candidate relation:

```orna
pub table User(id: Uuid) {
    username: Username,
    email: EmailAddress?,
    role: Role,

    assert all_unique(user => user.username);
}
```

The table body already identifies the constrained relation. The canonical form therefore omits both a repeated table name and an explicit `self |` pipeline.

```orna
pub table Booking(id: Uuid) {
    starts: Instant,
    ends: Instant,
    cancelled: Bool,

    assert every(booking =>
        booking.cancelled || booking.ends > booking.starts
    );
}
```

`all_unique` and `every` are ordinary predicate constructors supplied by the relational core/reference `std`; they are not grammar keywords. Other deterministic relation predicates may be composed in exactly the same way.

**ORNA-ASSERT-001** `assert` is the only assertion-introducing keyword in Orna 1.0.

**ORNA-ASSERT-002** Assertions are enforced in all modes and MUST NOT be removed from release builds.

**ORNA-ASSERT-003** A table assertion is lexically contained by its owning table.

**ORNA-ASSERT-004** The owner subject of a table assertion is `Relation<Row>` for the complete unpublished candidate relation.

**ORNA-ASSERT-005** The candidate relation includes every insert, update, delete and re-key pending in the current validation boundary.

**ORNA-ASSERT-006** `all_unique(selector)` returns a predicate over a relation and tests uniqueness of the selector result according to its declared equality/null policy.

**ORNA-ASSERT-007** `every(predicate)` returns a predicate over a relation and is true only when the row predicate is true for every row.

**ORNA-ASSERT-008** The exact relation-predicate library is extensible through ordinary functions; adding one does not add assertion grammar.

**ORNA-ASSERT-009** A table assertion expression MUST type-check as `Predicate<Relation<Row>>` after owner-subject elaboration.

**ORNA-ASSERT-010** Table assertions are deterministic for a fixed candidate relation and captured database snapshot.

**ORNA-ASSERT-011** A table assertion MUST NOT perform network, process, UI or arbitrary filesystem effects, use nondeterministic randomness, or observe uncaptured wall-clock time.

**ORNA-ASSERT-012** Multiple assertions are conjunctive and evaluated in source order for deterministic diagnostics.

**ORNA-ASSERT-013** A false table assertion aborts the validating boundary before the candidate relation becomes visible.

**ORNA-ASSERT-014** Writes from an assertion-aborted boundary are rolled back.

**ORNA-ASSERT-015** A table assertion MUST observe a transactionally coherent candidate relation and MUST NOT observe a partially committed state.

**ORNA-ASSERT-016** The source `assert self | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes `self |`.

**ORNA-ASSERT-017** The source `assert TableName | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes the repeated owner.

**ORNA-ASSERT-018** Dedicated table-field `unique` and `check(...)` modifiers are not 1.0 grammar. They migrate to table-owned `assert` predicates.

**ORNA-ASSERT-019** An assertion-specific `else` arm is invalid. Expected operational failure handling uses the ordinary failure/recovery model.

**ORNA-ASSERT-020** `ensure`, `check`, `fact`, `constraint`, `constraints`, dedicated `unique`, and `|!` are not aliases for `assert`.

### Algorithm ASSERT-TABLE-1: candidate-relation validation

Given table `T`, committed logical relation `C`, pending mutation set `M`, and ordered assertions `a1...an`:

1. Enter the storage engine's normal transaction or unpublished candidate-state boundary.
2. Derive `R = apply(C, M)` without publishing it.
3. Elaborate each assertion predicate against owner subject type `Relation<T>`.
4. Evaluate assertions in source order against `R`.
5. If predicate evaluation fails, abort the boundary and propagate that failure.
6. If a predicate is false, select a deterministic safe witness where feasible, emit the table-assertion diagnostic, and abort.
7. Continue the surrounding commit, merge, checkout, rewrite or publication algorithm only after every applicable assertion succeeds.
8. Publish `R` only when the entire enclosing boundary succeeds.

## Cross-table module assertions

Some invariants have no honest single-table owner. A module may declare a closed, deterministic proposition that depends on at least two distinct tables:

```orna
assert every(Invoice, invoice =>
    exists(Customer, customer => customer.id == invoice.customer_id)
);
```

A module assertion has no implicit `self` or relation subject. Its expression is an ordinary closed `Bool` over the unpublished candidate database.

**ORNA-ASSERT-021** A module assertion is valid only when its resolved dependency graph includes at least two distinct table objects.

**ORNA-ASSERT-022** A module assertion whose table dependencies all belong to one table is invalid and receives `ORNA-A091-003` with a fix to move it into that table.

**ORNA-ASSERT-023** A module assertion with no table dependency is invalid; module loading is not a compile-time execution facility.

**ORNA-ASSERT-024** A module assertion expression MUST type-check as a closed `Bool` and has no implicit owner subject.

**ORNA-ASSERT-025** Module assertions obey the same determinism and effect restrictions as table assertions.

**ORNA-ASSERT-026** A module assertion is evaluated whenever a validating boundary changes any table in its transitive dependency set.

**ORNA-ASSERT-027** Module assertions evaluate after all affected table-owned assertions and before publication of the candidate database.

**ORNA-ASSERT-028** Several applicable module assertions are ordered by owning module's stable ObjectId and then source span, providing deterministic first-failure behavior across modules.

**ORNA-ASSERT-029** A false module assertion aborts the complete candidate database boundary and rolls back all Orna-controlled writes in that boundary.

**ORNA-ASSERT-030** A cross-table assertion MUST NOT be attached arbitrarily to one table merely to avoid module syntax; ownership should reflect the proposition's dependency scope.

### Algorithm ASSERT-DATABASE-1: cross-table validation

1. Compute the unpublished candidate database after all pending writes and schema projection for the boundary.
2. Determine affected module assertions from stable dependency metadata.
3. Validate affected table assertions first.
4. Sort affected module assertions by stable owner identity and source span.
5. Evaluate each closed Boolean against the same candidate database snapshot.
6. On failure or false, abort the complete boundary and preserve a safe deterministic witness where feasible.
7. Publish table state, Git/CWD state and any coupled checkpoint only after all applicable assertions succeed.


## Permitted key values

Version 1.0 primary-key components may contain values with stable equality, total order and canonical encoding:

- `Bool`;
- `Int`;
- canonical `Decimal`;
- `Str`;
- `Uuid`;
- `Date`;
- `Instant`;
- payload-free enum variants;
- table-row references whose referenced primary key is itself permitted and canonically encodable;
- tuples composed from permitted key values.

**ORNA-KEY-003** `Float` MUST NOT be accepted as a primary-key component.

**ORNA-KEY-004** A key's logical type MUST NOT be restricted to the host filesystem's native filename rules.

**ORNA-KEY-005** A stored table reference MAY be a key component when the referenced table key is permitted and canonically encodable. The path encodes the referenced row key, not an embedded row.

**ORNA-KEY-006** `Range<T>` is not a special overlap-identity key in version 1.0. If used as ordinary stored data, overlapping ranges are valid. A future temporal-exclusion feature must use an explicit, separately specified table constraint rather than redefining equality.

This means v1 has no built-in declaration for a gap-permitting exclusive timeline such as employment or vehicle ownership. Such data may still be represented with an ordinary identity key plus `Range<T>` fields and validated by ordinary code, but kernel-enforced temporal exclusion is explicitly deferred rather than implementation-defined.

## Persisted rows and path projections

Keys are logical values, not host paths. New-format rows live in one ordered
row store. The virtual filesystem projects a table/key to a row directory
with `data.orna` and content fields using [VFS-1](#vfs). Legacy `key-path-v1`
loose-row rules are frozen in the compatibility archive and MUST NOT impose
case-folding/path-length restrictions on new-format table keys.

A row projection is data-only and schema-directed. Key fields are supplied by
the directory address; repeated keys in `data.orna` are rejected. Complete
interchange exports may include keys explicitly under their export profile.
No arbitrary row file is executed on load. Copying a projected row document
alone does not copy the referenced content.

## Mutations

```orna
let alice = Contact.insert({
    name: "Alice Smith",
    emails: ["alice@example.com"],
});

Contact.update(alice.id, {
    emails: ["alice@new.example"],
});

Contact.delete(alice.id);
```

**ORNA-MUT-001** Rows are values; mutations are operations on tables.

**ORNA-MUT-002** Successful mutations MUST become visible in CWD atomically at the enclosing activation boundary.

**ORNA-MUT-003** Ordinary mutations MUST NOT automatically create a human Git commit unless an explicitly enabled publication policy publishes the accepted batch.

**ORNA-MUT-004** Primary keys MUST be immutable through ordinary `update`.

**ORNA-MUT-005** An explicitly keyed table MUST provide the ordinary table operation `Table.rekey(old_key, new_key)`. It changes one row's primary key atomically within the current activation.

**ORNA-MUT-006** `rekey` MUST fail without changing CWD when the old key is absent, the new key already exists, the new key is invalid, or deferred referential validation fails.

**ORNA-MUT-007** Referential constraints are validated at activation commit. A program MAY update dependent references and re-key the target in the same activation; no invalid intermediate state is observable outside that activation.

**ORNA-MUT-008** Non-optional references use `restrict` by default. Orna v1 does not perform an implicit cascade. A caller that wants dependent references changed MUST update them explicitly in the same activation.

**ORNA-MUT-009** Re-keying a table with the implicit automatic integer key is prohibited. Such an identity is never rewritten; create a new row and delete the old row instead.

**ORNA-MUT-010** `sys.Change` and semantic diff MUST represent a successful explicit re-key as one `rekey` change containing the old and new keys, even if its physical representation is a VFS directory change or a page replacement.

**ORNA-MUT-011** A raw filesystem path rename is not authoritative evidence of semantic continuity. Unless it corresponds to a pending explicit `rekey` intent, it is interpreted as deletion plus insertion; tooling MAY suggest the explicit operation but MUST NOT silently guess.

## Stable semantic identity and rename

Persistent definitions have committed stable `ObjectId` values separate from their names and revision hashes.

**ORNA-OBJECT-001** Stable IDs MUST exist for databases, modules, tables, columns, functions, nominal types, units, currencies and pages.

**ORNA-OBJECT-002** Stable IDs MUST be stored in canonical committed metadata rather than source annotations.

**ORNA-OBJECT-003** `orna mv <old> <new>` and semantic LSP rename MUST preserve the target ObjectId, update affected source/storage mappings and leave reviewable CWD changes without committing automatically.

**ORNA-OBJECT-004** A manual unassisted rename that cannot be tied to committed identity metadata MUST be treated conservatively as delete plus create. Orna MAY offer explicit adoption but MUST NOT guess silently.

**ORNA-OBJECT-005** Retired IDs MUST NOT be reused for an unrelated definition.

## Stored table references

A table type used as a field denotes a stored reference to a row of that table:

```orna
pub table Vehicle(id: Uuid) {
    owner: directory.Contact,
}
```

**ORNA-REF-001** A stored reference contains database identity, table ObjectId, primary key and snapshot context as required; it does not embed a duplicate row.

**ORNA-REF-002** `vehicle.owner.key` is available without loading the target row. `vehicle.owner.name` resolves through the same snapshot context and may lower to a lookup or join.

**ORNA-REF-003** Non-optional references default to `restrict` on target delete or re-key. Weak/dangling references and automatic cascade are outside version 1.0.

## Filesystem projection and mutation equivalence

The VFS is an optional adapter over the same row engine. A completed valid
save of `Music/london/data.orna` performs one ordinary row replacement against
its captured version; a Blob file save replaces that field's immutable value.
There is no authoritative loose-row projection to synchronize. Filesystem
save, local row commitment and Git publication are distinct boundaries.
See EDIT-1 for durable candidates, invalid edits and editor limitations.

## Assertion categories

| Source owner | Required expression after elaboration | Implicit subject |
|---|---|---|
| Executable block | `Bool` | null |
| Refined type | `Predicate<Base>` | candidate base value |
| Table | `Predicate<Relation<Row>>` | complete candidate relation |
| Module | closed `Bool` with at least two table dependencies | null; reads candidate database |

**ORNA-ASSERT-031** An executable assertion evaluates its closed Boolean exactly once at the statement position.

**ORNA-ASSERT-032** A false executable assertion fails the activation with an `AssertionFailure` carrying the owning function, source span and safe values where available.

**ORNA-ASSERT-033** A subjectless refined comparison is semantic elaboration and MUST NOT be implemented as a textual rewrite introducing a source-visible global `self`.

**ORNA-ASSERT-034** The implicit owner subject exists only during refined/table assertion elaboration and MUST NOT leak into nested ordinary declarations, closures, exports or stored values.

**ORNA-ASSERT-035** Assertion name resolution otherwise follows ordinary lexical/module resolution.

**ORNA-ASSERT-036** An empty `assert;` is invalid and receives `ORNA-A091-011`.

**ORNA-ASSERT-037** Every assertion clause ends with `;` and a missing terminator receives `ORNA-A091-005`.

**ORNA-ASSERT-038** A declaration assertion whose predicate type is incompatible with its owner receives `ORNA-A091-004` showing expected owner subject and actual type.

**ORNA-ASSERT-039** An effectful or nondeterministic declaration assertion receives `ORNA-A091-007` identifying the forbidden effect.

**ORNA-ASSERT-040** An implementation MAY stop after the first false assertion in deterministic order unless an explicit diagnostic mode requests all independent failures.

## Validation boundaries

**ORNA-ASSERT-041** Refined assertions run whenever a base value is explicitly constructed or converted into the refined type.

**ORNA-ASSERT-042** Table and applicable module assertions run before ordinary transaction commit.

**ORNA-ASSERT-043** They also run before accepting a merge candidate that changes logical state.

**ORNA-ASSERT-044** They run before accepting checkout/reset state as current when logical state changes.

**ORNA-ASSERT-045** They run before graph publication, migration or a new physical root becomes visible.

**ORNA-ASSERT-046** Concurrent writers validate against the transactionally serialized candidate state selected by the storage engine.

**ORNA-ASSERT-047** A replayable stream checkpoint MUST NOT advance when an assertion aborts the item or batch transaction.

**ORNA-ASSERT-048** Git snapshots, indexed row pages and the embedded unpublished tail are representations of one logical state and MUST NOT bypass assertions.

**ORNA-ASSERT-049** Evaluation failure inside an assertion propagates through the normal failure model and aborts the same boundary as a false assertion.

**ORNA-ASSERT-050** Assertion validation MUST preserve transaction atomicity and MUST NOT publish a state for which only a subset of applicable assertions ran.

## Diagnostics and privacy

**ORNA-ASSERT-051** A failure diagnostic identifies the failed assertion source span and owning declaration/module.

**ORNA-ASSERT-052** A refined failure identifies the candidate value through safe presentation rules.

**ORNA-ASSERT-053** A table/cross-table diagnostic identifies the predicate and, when feasible, a deterministic witness.

**ORNA-ASSERT-054** Diagnostics MUST NOT reveal secrets or values marked non-presentable.

**ORNA-ASSERT-055** A machine-applicable rewrite SHOULD be attached when migration is unambiguous.

**ORNA-ASSERT-056** A fixer MUST NOT rewrite arbitrary pipelines or `self` uses outside a recognized declaration-assertion legacy form.

**ORNA-ASSERT-057** Assertion failure MUST NOT be encoded as a hidden stored Boolean column, synthetic row or implicit index.

**ORNA-ASSERT-058** An optimizer MAY implement `all_unique`, `every` and cross-table predicates through indexes, anti-joins or incremental plans only when results and deterministic witnesses equal the reference logical evaluation.

**ORNA-ASSERT-059** Assertion dependencies appear in `sys.Dependency` and assertion definitions in `sys.Assertion`.

**ORNA-ASSERT-060** Presentation, formatting and diagnostic witness selection MUST NOT affect equality, storage, Git hashes or canonical serialization.

## Required assertion diagnostics

| Code | Condition | Primary remedy |
|---|---|---|
| `ORNA-A091-001` | legacy refined `where self` | use a brace-delimited `assert` block |
| `ORNA-A091-002` | `assert self | ...` or repeated table owner | remove explicit owner pipeline |
| `ORNA-A091-003` | one-table module assertion | move assertion into that table |
| `ORNA-A091-004` | predicate incompatible with owner | supply predicate for shown owner subject |
| `ORNA-A091-005` | missing assertion semicolon | terminate with `;` |
| `ORNA-A091-006` | assertion-specific `else` | use ordinary failure/recovery flow |
| `ORNA-A091-007` | forbidden effect/nondeterminism | remove identified effect |
| `ORNA-A091-008` | executable/refined assertion false | inspect owner, proposition and safe value |
| `ORNA-A091-009` | table/cross-table assertion false | inspect deterministic safe witness |
| `ORNA-A091-010` | removed alias/mini-language | use sole `assert` form |
| `ORNA-A091-011` | empty assertion | provide proposition |
| `ORNA-A091-012` | zero-table module assertion | place check in executable/test code |
