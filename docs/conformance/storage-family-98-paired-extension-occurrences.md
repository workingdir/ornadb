# Storage family 98: paired restore depth identities across extension snapshot omissions

MERGE-1 is silent about reporting typed occurrence states for both members of
a snapshot path extension pair in each paired restore wave. The v1
`column_restore_paired_path_extension_occurrence_folds()` view uses the
slash-delimited text-key relation documented for family 96 and preserves the
prefix and extension as separate canonical identities.

For each side of the pair, every column, storm, and wave reports one of:

- `ColumnOmitted` when that stable column has no wave slot.
- `PathOmitted` when the column is present but the exact path is absent from
  all its depth fragments.
- `DepthLabels(labels)` for the local fragments containing that path.

The two occurrence states are computed independently. A present prefix does
not lend its depth to an omitted extension, and a returning extension keeps
its own label even when the prefix is absent. The projection does not modify
canonical row keys or storage merge behavior.
