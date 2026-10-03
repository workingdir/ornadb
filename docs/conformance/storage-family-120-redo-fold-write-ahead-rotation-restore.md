# Storage family 120: redo-fold identity across sparse write-ahead rotation chains

The read-only reference is silent on restoring a compacted sparse redo stream
when each frame binds a paired write-ahead log identity to its active segment
incarnation and the enclosing fold has a separate paired identity. The v1
policy keeps those identity scopes distinct and restores only observations
present in the compacted runs.

- Each stream includes caller-known checkpoint IDs and IDs observed on either
  side of any supplied frame. Every frame contributes one slot to each stream,
  including streams omitted by both checkpoint sides.
- Slots carry the enclosing fold's exact directional identity, the exact
  left/right checkpoint values, and the frame's atomic log/segment pair.
  `(fold_ordinal, order)` distinguishes repeated fold labels and overlapping
  orders across folds.
- Compaction joins only adjacent orders in the same fold with equal checkpoint
  state and fold identity. Log or segment rotation does not split the state
  run; every paired log/segment identity remains in order on the compacted run.
- Restore expands a run only when it has one paired log/segment identity per
  represented order. Invalid ranges and truncated or overlong identity vectors
  return an error. Gaps, checkpoint omissions, positionless values, fold
  boundaries, and repeated identity pairs are preserved without synthesis.

The focused proof constructs its checkpoint values, paired fold labels, log
IDs, and segment incarnations from `.orna` fixtures included inside the storage
crate with `include_str!`. It checks exact fold/compression/restore equality,
rotations within a compacted run, a sparse order gap, absent and late-observed
streams, a present positionless checkpoint, and rejection of a truncated
rotation vector.
