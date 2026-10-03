# Storage family 101: sparse checkpoint stream identity across redo fold omissions

MERGE-1 does not specify how a known checkpoint stream survives a fold when
the stream is omitted from both sides, including when it is omitted from every
frame in the fold. The v1
`fold_paired_checkpoint_redo_sparse_streams()` projection carries a caller's
known checkpoint IDs into the output and unions them with IDs observed in the
frames. This preserves sparse identity without treating an omitted checkpoint
as a deletion or requiring a dense input representation.

- Every supplied redo order produces one slot for each output stream ID.
- Missing left and right entries remain independent `None` values; a present
  positionless checkpoint remains `Some(CheckpointGeneration { position:
  None, .. })`.
- A checkpoint omitted from both sides retains its stream ID and the frame's
  paired write-ahead identity. A known stream omitted from the whole fold has
  an all-`None` slot history rather than disappearing.
- Frame order gaps remain gaps: only supplied frame orders are represented.
- An observed ID absent from the caller's catalog is included, so stale
  catalogs cannot discard checkpoint data.

This is a read-only projection. It does not infer checkpoint values, merge
log identities, or interpret opaque IDs and positions.
