# 25. Schema evolution, semantic diff and merge {#evolution}

## No migration files

**ORNA-MERGE-001** The semantic difference between two valid snapshots is the migration between them.

**ORNA-MERGE-002** Orna MUST NOT require an independent SQL-style migration file merely to express a declarative schema change.

## Rename

**ORNA-SCHEMA-001** Semantic rename uses stable ObjectIds and `orna mv`; old snapshots retain old names mapped to the same identity.

**ORNA-SCHEMA-002** A plain rename without identity continuity is delete plus create.

## Adding and changing fields

Adding an optional field is metadata-compatible:

```text
email: Str?
```

Older rows read `null`.

A stored default is insertion behavior:

```text
country: Str = "GB"
```

New rows that omit `country` receive and logically store the value produced once at insertion.

For rows that predate the field, a closed constant default may be committed as a frozen field-introduction fallback. The fallback value is pinned to that schema revision. Later changing the declaration to `"US"` affects future inserts only; it does not change historical rows that previously resolved to `"GB"`.

A row-dependent default may be used for future insertions, but existing rows require an explicit backfill. Orna never turns a stored field into an undocumented compute-on-read field.

A computed field is declared separately:

```text
full_name: Str => "{first} {last}"
```

It is a read-only row-local selector, not stored data. It automatically changes when `first` or `last` changes and may not be supplied or updated directly.

**ORNA-SCHEMA-003** Optional additions MUST NOT require rewriting every existing row.

**ORNA-SCHEMA-004** An insert-time default is evaluated once for future rows. A frozen introduction fallback for older rows MUST be stored in committed schema metadata and MUST NOT change when the source default expression is later edited.

**ORNA-SCHEMA-005** Only a closed deterministic constant may become an implicit fallback for rows that predate a field. Row-dependent or effectful defaults require explicit backfill for existing rows.

**ORNA-SCHEMA-006** A required stored field without a complete default/backfill is invalid while existing rows lack a value.

**ORNA-SCHEMA-007** A computed field is not a migration/backfill mechanism and MUST follow the computed-field rules in [the tables chapter](#tables).

A backfill is ordinary Orna code, not a migration language. Large backfills may use checkpointed batches.

## References

**ORNA-SCHEMA-008** Non-optional stored references default to `restrict` on delete/re-key. Automatic cascade is outside version 1.0.

The program may update dependants and delete the target within one ordinary activation transaction.

## Three-way merge algorithm MERGE-1

1. Resolve merge base, ours and theirs through Git.
2. Parse and resolve each module graph and stable ObjectIds.
3. Perform source-aware and definition-aware three-way merges.
4. Derive the candidate merged schema.
5. For every table, compare editable-tree or compact-manifest/content digests before reading rows:
   - if ours equals theirs, reuse it;
   - if ours equals base, take theirs;
   - if theirs equals base, take ours;
   - otherwise descend only into changed and overlapping key ranges.
6. Merge changed editable rows and compact logical mutations by primary key.
7. Validate candidate rows against candidate schema, references and constraints.
8. Rebuild dependency impact for affected definitions.
9. Type-check affected functions, pages and stream programs.
10. Produce a valid isolated merge result or typed conflicts. Do not partially mutate ordinary CWD.
11. Move the isolated result into CWD only after it is complete and within configured resource/conflict budgets.

**ORNA-MERGE-003** An implementation MUST use manifest, tree or segment digest equality to skip identical subtrees before decompressing or comparing their rows.

**ORNA-MERGE-004** Merge work SHOULD be proportional to changed or overlapping key ranges rather than total table size where the storage profile exposes suitable digests and bounds. A genuinely table-wide change may still require table-wide work.

**ORNA-MERGE-005** A merge MUST have explicit resource and conflict-count budgets. If a budget is exceeded, the operation stops with CWD unchanged and reports the affected tables/ranges plus a lower bound on discovered conflicts. It MUST NOT exhaust memory or create millions of materialized conflict objects silently.

**ORNA-MERGE-006** Distinct compatible optional columns SHOULD merge automatically.

**ORNA-MERGE-007** Incompatible edits to the same column type MUST produce a schema conflict.

**ORNA-MERGE-008** Independent edits to different fields of one keyed row SHOULD merge automatically.

**ORNA-MERGE-009** Different edits to the same field MUST produce a row conflict unless an explicit type-specific merge rule exists.

**ORNA-MERGE-010** Generated-key and automatic-ID collisions are normal row conflicts and MUST NOT be silently renamed.

**ORNA-MERGE-011** Divergent opaque checkpoints MUST produce `sys.CheckpointConflict` rather than a guessed merge.

## Semantic diff

```text
$ orna diff main..feature

directory.Contact
  + column phone: Str?
  ~ 2 rows

energy.Tariff
  + 2026-09

energy.daily()
  affected by energy.Tariff

/energy
  affected through energy.daily()
```

**ORNA-DIFF-001** Semantic diff SHOULD report schema, keyed row, result and dependency changes where available.

**ORNA-DIFF-002** Raw Git diff MUST remain available.

**ORNA-DIFF-003** Pure physical representation changes MUST be distinguishable from logical data changes.

