# Storage family 119: redo-fold identity across sparse checkpoint restores

The read-only storage reference does not define restoring per-order sparse
checkpoint streams from compacted fold runs. The v1 policy treats each
compacted inclusive order range as the exact set of observed frames it
represents and restores each slot with the run's complete checkpoint state and
paired redo-fold identity.

- Restored slots retain their original fold ordinal and order, so repeated
  order numbers across folds remain distinct.
- Each order inside a run expands to one slot with the same left/right
  checkpoint generations and directional fold pair.
- Orders between runs remain absent; restoration does not infer frames across
  gaps or across fold boundaries.
- Stream order and checkpoint identity are preserved, and present
  positionless checkpoint generations remain `Some` values.

The focused behavior proof constructs sparse fold streams from in-crate
`.orna` fixtures included with `include_str!`, compacts them, restores them,
and asserts full equality with the original slots plus explicit gap, fold, and
fixture-value checks.
