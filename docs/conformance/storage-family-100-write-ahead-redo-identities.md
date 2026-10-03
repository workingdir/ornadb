# Storage family 100: paired write-ahead identity preservation across redo chain compactions

The family 99 compressor coalesces repeated paired checkpoint values. MERGE-1
does not specify how source write-ahead log identities survive that
compaction. The v1 `compress_paired_checkpoint_redo_chain_preserving_write_ahead_identity()`
projection retains one ordered `(left_log, right_log)` identity pair for every
input redo order inside each compressed checkpoint run.

- Checkpoint runs still coalesce only across consecutive orders with exactly
  equal state on both sides. A side change or order gap starts another run.
- Log identity changes do not block state compaction; all original identity
  pairs remain attached in commit order, and repeated pairs are not deduplicated.
- Left and right log identities remain directional and opaque byte strings.
- Each output chain retains its checkpoint ID, and each run retains its order
  range and both checkpoint values.

This read-only v1 view preserves redo provenance without interpreting IDs,
rewriting checkpoints, or combining the two logs.
