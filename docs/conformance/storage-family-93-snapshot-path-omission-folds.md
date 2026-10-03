# Storage family 93: snapshot path omission identities across depth folds

MERGE-1 is silent about folding a canonical snapshot path back through the
per-column restore depth labels. The v1
`column_restore_storm_snapshot_path_folds()` projection emits one record per
stable `(table, column, snapshot_path)` identity in each restore storm:

- Each record carries its storm range and dense restore-wave slots.
- A column omitted from a wave has `depth_labels: None`.
- A present column where this path is absent has `Some(Vec::new())`.
- A path present in a wave lists every local depth label where it occurs;
  equal paths in sibling columns or other storms remain separate identities.

The fixture proof follows the same snapshot path through a present wave, a
wave where the paired column is present but the path is omitted, and a wave
where that path returns. It also checks that omission of the sibling column is
represented separately from omission of a path within a present column.
