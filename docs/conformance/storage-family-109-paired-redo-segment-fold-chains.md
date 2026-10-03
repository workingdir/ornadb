# Storage family 109: paired redo identity across sparse segment fold chains

The reference defines checkpoint and segment identities, but does not specify
how multiple sparse redo folds compose when their in-fold orders overlap. The
v1 projection chains folds in caller order and tags each output slot with its
fold ordinal and original order.

- The caller's checkpoint catalog is unioned with IDs observed in all folds.
  Streams first observed in a later fold are retained.
- Every supplied frame contributes one slot per output stream. The exact
  left/right checkpoint values and paired segment incarnation are preserved,
  including when a stream is absent on both sides.
- `(fold_ordinal, order)` identifies a slot, so equal order numbers in
  different folds remain distinct. Gaps inside each fold remain gaps.
- Segment pairs are copied per occurrence without deduplication or
  interpretation. Repeated physical labels only identify the same segment
  incarnation when their supplied tokens are equal.

This is a read-only v1 composition policy; it does not synthesize redo frames
or rewrite segment history.
