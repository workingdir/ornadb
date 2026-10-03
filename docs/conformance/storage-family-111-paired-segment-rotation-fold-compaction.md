# Storage family 111: paired segment rotation identity across sparse checkpoint fold chains

The reference requires compaction to preserve semantic rows, keys, and
checkpoints, but does not define a compacted projection over chained sparse
checkpoint folds or specify how segment rotation identity is carried by that
projection. The v1 policy compacts equal checkpoint state within each fold
while retaining the paired segment identity for every represented order.

- The input catalog and stream order are retained, including streams with no
  observed checkpoint values and streams first observed in a later fold.
- A run joins only adjacent numeric orders in the same fold when both sides'
  checkpoint values are equal. `None` remains distinct from a present
  positionless checkpoint.
- Every input order contributes its exact left/right segment pair in order.
  Rotation and repeated pairs therefore survive state compaction.
- Gaps, state changes, and fold boundaries start new runs. Overlapping order
  numbers in separate folds remain separate even when their checkpoint state
  is equal.

This projection does not infer omitted orders or coalesce across fold
boundaries. The focused proof builds its paired identities from in-crate
`.orna` fixtures with `include_str!`.
