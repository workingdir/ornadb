# Storage family 112: paired compaction fold identity across sparse undo-chain rotation folds

The reference says cancelling a stream does not undo previously committed
deliveries, but does not specify how paired undo-chain identities are
projected or compacted across sparse checkpoint folds. The v1 policy retains
every directional undo pair while compacting equal checkpoint state locally
within each fold.

- The stream catalog includes caller-known checkpoint IDs and IDs observed in
  any fold; every frame contributes a slot to every stream.
- Compacted runs join only adjacent orders in one fold when both checkpoint
  sides are equal. `None` remains distinct from a present positionless value.
- Each order's exact left/right undo-chain pair is retained in order, even
  when identities rotate or repeat within a run.
- Fold boundaries, sparse order gaps, and checkpoint-state changes split runs.
  Equal order numbers in separate folds therefore remain distinct.

This projection does not infer missing frames, undo committed deliveries, or
use an undo-chain rotation to discard or suppress an identity. The behavior
proof reads its `.orna` identity fixtures with `include_str!` from inside the
storage crate.
