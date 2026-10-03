# Storage family 91: snapshot path identity across restore storm folds

The storage reference is silent about exposing snapshot paths alongside the
depth labels in paired column restore storms. The v1
`column_restore_storm_depth_paths()` projection uses each canonical row key as
the snapshot-path identity:

- Each record carries the stable `(table ObjectId, column ObjectId)` pair,
  storm index and inclusive range, wave order, and local fragment label.
- `snapshot_paths` lists the exact canonical row keys in that labeled
  fragment, in the order retained by the restore fold.
- A labeled empty fragment yields an empty path list. An omitted column or
  wave yields no record and cannot borrow paths from a neighboring label.
- Repeated row keys remain paired with their own label, wave, column, and
  storm; path identity is not inferred from the field value.

This keeps snapshot ancestry inspectable through uneven restore folds without
merging same-path occurrences from separate labels or storms.
