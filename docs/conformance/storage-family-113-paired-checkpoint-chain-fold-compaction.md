# Storage family 113: paired checkpoint chain identity across sparse compaction fold rotations

The reference specifies stable consumer/checkpoint identity and typed opaque
checkpoint positions, but does not define redo-chain label comparison or
compaction across sparse folds. The v1 policy carries each frame's paired redo
chain identity through fold projection and compacts only locally stable runs.

- The output catalog unions known checkpoint IDs with IDs observed in any
  fold. Every frame contributes one slot per stream, including omissions.
- A run joins only adjacent orders in the same fold when both checkpoint
  values and the paired redo-chain identity are equal.
- Each run retains the atomic paired log/segment identity for every order.
  Log or segment rotation therefore does not split a stable redo-chain run.
- Fold boundaries, sparse order gaps, checkpoint-state changes, and redo-chain
  changes start new runs. Overlapping orders in separate folds remain distinct.
- Missing checkpoint values remain distinct from present positionless values;
  later-observed streams keep their earlier omission slots.

The projection does not infer missing orders or interpret opaque chain labels
as timestamps. Its focused behavior proof parses the existing in-crate `.orna`
fixtures through `include_str!` and asserts their real paired values.
