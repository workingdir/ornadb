# Storage family 94: snapshot depth labels across chained restore folds

MERGE-1 is silent about retaining one snapshot-path identity across multiple
restore storms. The v1 `column_restore_snapshot_path_chain_folds()` projection
groups by stable `(table, column, canonical row key)` and preserves each
storm's local structure:

- Every storm remains a separate entry with its committed order range.
- Every restore wave in a storm retains its path occurrence labels.
- `depth_labels: None` means the column was omitted from that wave;
  `Some(Vec::new())` means the column was present but that path was absent.
- A path that reappears in a later wave or storm keeps the same canonical key
  while its depth labels remain local to the new wave.

The fixture proof follows the same key through three storms, including an
entire storm where the paired column is omitted and a later return at a new
depth label.
