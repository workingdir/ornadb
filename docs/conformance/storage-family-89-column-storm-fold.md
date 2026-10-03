# Storage family 89: column depth labels across restore storms

The storage reference is silent about a stable column's depth labels when
committed column-restore storms are separated by another submission mode. The
adapter exposes `column_restore_storm_folds()` with these v1 semantics:

- A stable column is identified by `(table ObjectId, column ObjectId)`.
- Every committed column-restore storm remains an independent group in that
  column's fold. Fragment labels restart within each wave and do not continue
  across a non-column submission boundary.
- Each group has one slot per restore wave in that storm. A present empty
  fragment remains labeled; an omitted column is `None`.
- If a stable column is absent from an entire storm, its fold still contains
  that storm's order slots, all marked `None`.

This keeps paired column labels correlated across grouped restores while
preventing a label from one storm from being reused as another storm's depth.
