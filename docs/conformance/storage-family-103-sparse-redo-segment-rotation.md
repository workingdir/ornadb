# Storage family 103: write-ahead segment rotation identity across sparse redo folds

MERGE-1 does not define how active write-ahead segment identity survives
sparse redo folds when either side rotates segments. The v1
`fold_paired_checkpoint_redo_sparse_streams_preserving_segment_rotation_identity()`
projection attaches each frame's directional left/right segment tokens to
every checkpoint stream slot at that order.

- Checkpoint IDs from the caller's catalog are unioned with IDs observed in
  frames, preserving streams omitted throughout a fold and retaining observed
  streams omitted from a stale catalog.
- Each segment token identifies one segment incarnation. A rotation uses a
  new token, including when the underlying physical label is reused. The
  projection preserves the exact pair without interpreting it.
- Segment identity remains present when a checkpoint stream is omitted on one
  or both sides. Checkpoint absence remains distinct from a present
  positionless value.
- Repeated segment pairs are retained at every supplied order. Order gaps
  remain gaps and no segment or checkpoint event is inferred.

This read-only v1 projection preserves rotation lineage without rewriting
checkpoint state or segment identities.
