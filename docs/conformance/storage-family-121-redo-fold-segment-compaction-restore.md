# Storage family 121: redo-fold identity across sparse segment compaction restores

The read-only reference is silent on restoring paired redo-fold identity from
compacted sparse checkpoint runs that retain per-order segment incarnations.
The v1 policy restores only represented orders and binds each one to its exact
checkpoint state, directional fold pair, and directional segment pair.

- Runs expand to slots only when the order range is valid and the run carries
  exactly one segment identity pair for every represented order. Malformed
  ranges and short or overlong identity vectors return errors.
- Restored slots retain their fold ordinal and are ordered by
  `(fold_ordinal, order)`. Reused fold identities across different ordinals
  therefore remain distinct, and overlapping redo orders do not collide.
- Sparse gaps and fold boundaries remain absent/bounded; compaction cannot
  invent an order while restoring a run. Checkpoint omissions remain `None`,
  while present checkpoint generations (including positionless values) retain
  their original values.

The focused proof folds and compacts in-crate `.orna` fixture data with
`include_str!`, restores it, and compares the full restored streams to their
input values. It also checks omission/order-gap boundaries and proves that a
truncated segment identity vector is rejected instead of losing a rotation.
