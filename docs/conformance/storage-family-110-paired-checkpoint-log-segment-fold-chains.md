# Storage family 110: paired checkpoint identity across sparse segment rotation chains

The reference describes checkpoint, write-ahead, and segment identities, but
does not specify a chained sparse projection that preserves their binding
across folds whose in-fold orders overlap. The v1 projection concatenates
folds in caller order and identifies each slot by `(fold_ordinal, order)`.

- The stream catalog includes both caller-known checkpoint IDs and IDs
  observed in any fold, including streams that appear only later.
- Every supplied frame contributes one slot per stream with its exact
  left/right checkpoint values and complete paired `(left_log, right_log,
  left_segment, right_segment)` identity.
- Equal order numbers in different folds remain distinct. Gaps inside a fold
  stay visible, and repeated composite lineage pairs are retained at each
  occurrence.
- Omitted checkpoint streams retain their slot and paired lineage. A present
  positionless checkpoint remains distinct from absence.

This read-only v1 composition policy does not infer checkpoint or rotation
events and does not split or recombine the supplied log/segment identity pair.
