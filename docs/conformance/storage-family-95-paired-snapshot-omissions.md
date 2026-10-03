# Storage family 95: restore omission identities across paired snapshot depth folds

MERGE-1 is silent about aligning one canonical snapshot path across the stable
columns in a table. The v1
`column_restore_paired_snapshot_path_chain_folds()` projection emits one record
per `(table, snapshot_path)` identity and includes every stable column for that
table, even when the path never occurs in one of those columns.

Each column retains its full restore-storm chain and local depth labels:

- An omitted column wave is `None`.
- A present column where the path is absent is `Some(Vec::new())`.
- A present path records every fragment label that contains it.
- Storm boundaries and wave orders remain attached to each column, so a path
  or label cannot shift into a sibling column or a later restore.

This paired view aligns only the canonical row-key path. It preserves each
column's independently observed omission and depth history instead of
inferring one column's state from another.
