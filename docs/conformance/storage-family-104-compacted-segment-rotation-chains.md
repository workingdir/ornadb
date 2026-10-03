# Storage family 104: paired compaction identity across sparse segment rotations

MERGE-1 is silent on coalescing sparse checkpoint redo states while preserving
write-ahead segment rotations. The v1
`compress_paired_checkpoint_redo_sparse_chains_preserving_segment_rotation_identity()`
projection coalesces only adjacent orders with the same full left/right
checkpoint state and retains one ordered left/right segment identity pair for
every input order in each run.

- Caller-known checkpoint IDs are unioned with observed IDs. A known stream
  omitted throughout the input remains in the result with explicit absent
  state runs.
- State compaction requires consecutive orders and equal states on both
  sides. One-sided absence, both-sided absence, and a present positionless
  checkpoint remain distinct states.
- Segment rotation does not block a checkpoint-state run from compacting; all
  segment identity pairs remain in the run in commit order, including repeated
  pairs.
- Checkpoint state changes and order gaps split runs. No unobserved orders or
  values are synthesized.

The segment incarnation tokens remain opaque and directional. The projection
compacts checkpoint state only; it preserves the rotation chain without
interpreting or rewriting segment identities.
