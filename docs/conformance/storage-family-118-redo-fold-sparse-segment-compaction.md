# Storage family 118: redo-fold identity across sparse segment compaction

The read-only storage reference does not specify how a fold-level redo pair
composes with sparse per-order redo-chain, log/segment, and compaction identity.
The v1 policy preserves each at its source scope: redo-fold identity is carried
by the enclosing fold, while the remaining lineage stays attached to the frame
and original order.

- The checkpoint catalog unions known IDs with IDs observed in any frame.
  Every input order projects a slot to every stream, including omissions.
- Each slot carries the exact enclosing redo-fold pair, paired redo-chain
  identity, atomic log/segment pair, and optional compaction observation.
  `(fold_ordinal, order)` distinguishes repeated order numbers across folds.
- Compaction joins adjacent in-fold orders only when both checkpoint states,
  fold identity, and redo-chain identity are equal. Optional compaction
  observations do not split a run; the exact optional values and log/segment
  pairs remain in order on the compacted run.
- Gaps, fold boundaries, checkpoint changes, and redo-chain changes split
  runs. A missing compaction observation is retained as `None`, and a present
  positionless checkpoint remains distinct from a missing checkpoint.

The focused proof reads the fold, redo-chain, log, segment, compaction, and
checkpoint values from `.orna` fixtures included inside the storage crate with
`include_str!` and checks the real values at both slot and compacted-run level.
