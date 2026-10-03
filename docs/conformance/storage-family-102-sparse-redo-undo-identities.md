# Storage family 102: paired undo-chain identity across sparse redo folds

MERGE-1 does not define how paired undo-chain provenance survives sparse redo
folds. The v1
`fold_paired_checkpoint_redo_sparse_streams_preserving_undo_chain_identity()`
projection attaches the frame's directional `(left_chain, right_chain)` pair to
each stream slot at that committed order.

- A caller-supplied checkpoint catalog is unioned with checkpoint IDs observed
  in the frames, retaining streams omitted throughout the fold and preventing
  stale catalogs from hiding observed streams.
- The paired undo identity remains attached when a stream is omitted on one or
  both sides. Each output slot preserves the exact left/right checkpoint
  values, including the distinction between absence and a present
  positionless checkpoint.
- Repeated undo identity pairs are retained at each supplied order; they are
  not collapsed or deduplicated by sparse compaction.
- Frame order gaps remain gaps. The fold represents supplied committed orders
  only and does not infer missing undo or checkpoint events.

This read-only projection treats undo identities as opaque directional bytes.
It does not interpret, merge, or rewrite undo chains.
