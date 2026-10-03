# Storage family 90: paired labels across column restore storms

The storage reference is silent about the direct label projection of grouped
column-restore storms. `column_restore_storm_depth_labels()` returns one entry
per present stable table-column ladder and restore-wave order:

- Each record carries its `(table ObjectId, column ObjectId)` identity, the
  zero-based committed storm index, the storm's inclusive order range, and the
  wave order.
- `depth_labels` lists the wave's local labels in ascending order. Labels
  restart for each wave and are not aligned across sibling columns or storms.
- A labeled empty fragment contributes its label. An omitted column has no
  record for that wave.
- Storm indices follow committed lineage order and do not depend on which
  column is being inspected.

This v1 view lets consumers pair labels by stable column, storm, and wave
without deriving identity from cell contents.
