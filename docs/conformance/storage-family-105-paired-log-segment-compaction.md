# Storage family 105: paired log and segment identity across sparse compaction

MERGE-1 is silent on preserving the binding between paired write-ahead log
IDs and their active segment incarnations while sparse redo state is
compacted. The v1
`compress_paired_checkpoint_redo_sparse_chains_preserving_log_segment_identity()`
projection stores the four directional IDs as one per-order lineage record in
each compacted run.

- The checkpoint catalog is unioned with IDs observed in redo frames. Known
  streams omitted throughout the fold remain represented.
- Runs combine only consecutive orders with equal full left/right checkpoint
  state. One-sided absence, two-sided absence, and present positionless values
  remain distinct states.
- Log and segment identities are retained together in commit order. A segment
  rotation or log-ID change does not block equal checkpoint states from
  compacting, and repeated lineage records are not deduplicated.
- A state change or order gap starts a new run. No missing state or identity
  record is inferred.

The projection treats log and segment IDs as opaque directional bytes. It
preserves their association without interpreting or rewriting either ID.
