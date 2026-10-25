# 8. Relations, queries and historical evaluation {#relations}

## Relation values and bounded observation

A table expression produces `Relation<RowType>`.

```orna
directory.Contact
    | filter(c => c.name.starts_with("A"))
    | sort_by(c => c.name)
```

**ORNA-REL-001** A relation value MUST remain composable without requiring full materialization.

**ORNA-REL-002** Relation presentation MAY request a bounded window of rows without enumerating the whole relation.

A submitted relation expression has been evaluated to a relation value even when only a window is enumerated. REPL previews MUST communicate estimates visually, for example `≈42.1k rows`, rather than describing the value as “not executed”.

## Observable relation order

**ORNA-ORDER-001** A base table scan MUST be ordered by ascending canonical primary key.

**ORNA-ORDER-002** `filter` and one-to-one `map` preserve input order; `flat_map` uses input order followed by each produced value's order; `take`, `drop`, windows and `pairs` require/preserve order.

**ORNA-ORDER-003** `sort_by` establishes a stable order, `distinct` keeps the first occurrence, `union` yields left then right, grouped results use group-key order, and joins preserve left order with each right match in right-key order.

**ORNA-ORDER-004** An optimizer may reorder internally but MUST restore the declared observable order before values are observed.

## Equality and relation comparison

Values compare structurally or nominally according to their types. Decimal equality ignores representational scale. Row values compare table ObjectId, primary key, snapshot context and logical fields. Stored row references compare target database/table identities and key.

**ORNA-EQ-001** Relation-wide `==` is invalid because sequence equality and set-of-rows equality are different operations. Libraries may provide explicitly named operations such as `same_sequence` and `same_rows`.

## Reusable derived data

```orna
pub fn readings_above(limit: Decimal) =
    energy.Reading
        | filter(reading => reading.value > limit);
```

**ORNA-QUERY-001** A named live calculation MUST be an ordinary function rather than a `view` declaration.

**ORNA-QUERY-002** Calling the function evaluates its logical query against the current context unless arguments are explicitly snapshot-pinned.

**ORNA-QUERY-003** Two calls are not implicitly collapsed merely because their source spelling and arguments match. Effectful or nondeterministic calls execute independently.

**ORNA-QUERY-004** An implementation MAY reuse a previous call result only when the function is internally proven read-only and deterministic and its code, arguments, snapshot context, activation time and data dependencies have not changed. Reuse is an optimization, not a semantic guarantee.

**ORNA-QUERY-005** Writes performed earlier in the same activation participate in dependency invalidation and read-your-writes. A later call MUST observe those writes when its dependencies include the changed table.

**ORNA-QUERY-006** Actual reuse/materialization decisions SHOULD be visible through `sys.Materialization` and `orna explain` without requiring user effect annotations.

## Current code over historical data

```orna
energy.Reading.as_of(sys.snapshot("HEAD~10"))
```

resolves the current table definition and current calling code while reading compatible data from the selected commit.

**ORNA-HIST-001** Data pinning MUST NOT silently switch the entire executing program to historical code.

## Whole-program historical evaluation

```text
let old = sys.database.as_of(sys.snapshot("HEAD~10"));

old.energy.daily()
old.directory.Contact
old.sys.Table
```

The returned database snapshot object is a read-only root namespace for the historical code, schemas, rows, attached database pins, language edition, `std` commit and committed semantic metadata of that snapshot.

**ORNA-HIST-002** Calling a function through a database snapshot object invokes the historical definition.

**ORNA-HIST-003** Historical snapshot execution MUST NOT mutate current CWD, advance live checkpoints, use current secrets, open connectors or perform external effects.

**ORNA-HIST-004** Values from different snapshot contexts MUST NOT be mixed implicitly.

**ORNA-HIST-005** If required historical Unicode, time-zone, codec or edition support is unavailable, Orna reports incomplete reproducibility rather than silently substituting current semantics.

## Materialization and optimization

There is no user-visible `store` or `cache` declaration in version 1.0.

**ORNA-PLAN-001** Materialization, caching, indexing and incremental maintenance MUST NOT change logical function results.

**ORNA-PLAN-002** Planner choices MUST be introspectable through `sys.Plan`, `sys.Storage`, `sys.Materialization` and `orna explain`.

**ORNA-PLAN-003** Every optimization MUST have a correct scan/reference fallback.

Versioned planner hints are deferred until real workloads establish a useful policy vocabulary.

## Activation consistency and call reuse

An activation already captures one starting CWD generation, snapshot context and `now()` value while observing its own writes. This gives consistency; it does not imply that every function is memoized.

**ORNA-CALL-001** Two calls in one activation that are proven read-only and deterministic, have equal arguments, use the same code revision/context and observe unchanged dependencies MUST return observationally equal values.

**ORNA-CALL-002** A write, checkpoint movement, external observation or any dependency change between calls invalidates reuse. Effectful or nondeterministic calls MUST NOT be collapsed merely because their source and arguments look equal.

**ORNA-CALL-003** An implementation MAY evaluate an eligible call once and reuse the value. It MAY instead evaluate it repeatedly, provided the observable result and dependency context are identical.

**ORNA-CALL-004** When reuse/materialization occurs, `sys.Query`, `sys.Plan` or `sys.Materialization` MUST make the decision inspectable. The language does not promise a particular evaluation count for pure calls.

Example:

```orna
let before = Contact | count;
Contact.insert({ name: "Alice" });
let after = Contact | count;       // sees the write; cannot reuse `before`
```

## Dependencies and lineage

The compiler/runtime maintains exact dependency edges where statically knowable and observed edges where dynamic.

**ORNA-DEP-001** Dependencies MUST identify source and destination objects with typed `sys.Object` references.

**ORNA-DEP-002** Dependency kinds SHOULD include imports, calls, reads, writes, references, renders, uses-type and uses-unit.

**ORNA-DEP-003** Dependency information MUST be used for impact analysis, live invalidation, semantic diff, merge validation and selective testing where applicable.

## Attached databases and pinning

**ORNA-ATTACH-001** Cross-database reads MAY compose relations from consistent snapshots of each attached database.

**ORNA-ATTACH-002** Orna MUST NOT claim atomic cross-database writes when attached databases have independent transaction logs.

**ORNA-ATTACH-003** Structural unit equivalence MUST be checked across database boundaries.

**ORNA-PACKAGE-001** Version 1.0 MUST NOT require a package registry, semantic-version solver or lockfile where exact Git commit pins already determine dependency state.

**ORNA-PACKAGE-002** Checking out an historical parent snapshot MUST resolve each attached database to the commit pinned by that snapshot.

**ORNA-PACKAGE-003** `std` remains an ordinary optional attached Orna database/library except for mandatory core language and `sys` facilities.

