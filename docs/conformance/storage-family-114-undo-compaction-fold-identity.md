# Storage family 114: paired undo identity across sparse compaction rotations

The reference discusses checkpoint identity and opaque positions, but does not
define paired undo-chain and compaction identity interactions during sparse
fold compaction. The v1 storage policy retains both directional pairs for each
order while compacting equal checkpoint states only within one fold.

- The stream catalog unions caller-known checkpoint IDs with IDs observed in
  any fold. Each frame contributes a slot to every stream, including omissions.
- Compacted runs join adjacent orders within the same fold when both checkpoint
  sides have equal full generations. Undo-chain or compaction rotations do not
  split equal checkpoint state; each exact pair is retained at its order.
- Fold boundaries, checkpoint-state changes, and sparse order gaps split runs.
  Reused order numbers in different folds therefore remain distinct.
- The undo and compaction identity vectors are parallel: each element records
  the left/right pair for the corresponding order, preserving direction and
  repeated values without inferring missing frames.
- Missing checkpoint values remain distinct from present positionless values;
  later-observed streams retain their earlier omission slots and lineage.

The behavior proof parses the paired identity and checkpoint `.orna` fixtures
with `include_str!` from inside the storage crate and asserts computed states,
fold coordinates, and identity values.
