# Storage family 97: snapshot omission path identities across paired depth restores

MERGE-1 is silent about a typed occurrence state for one canonical snapshot
path across paired column restore waves. The v1
`column_restore_paired_snapshot_path_occurrence_folds()` view preserves the
path identity and makes three states explicit:

- `ColumnOmitted` means the stable column has no slot in that wave.
- `PathOmitted` means the column is present but the snapshot path occurs in no
  depth fragment for that wave.
- `DepthLabels(labels)` records every local fragment label containing the
  path. Labels remain local to their column, storm, and wave.

The paired fold includes the path identity even when both columns omit it.
Storm ranges and wave orders remain attached to each stable column, so an
omission cannot shift a path into another depth slot. This is a read-only
projection: canonical row keys and restore behavior are unchanged.
