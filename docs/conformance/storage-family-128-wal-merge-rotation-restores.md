# Storage family 128: sparse WAL merge rotation restores

The reference does not specify restoration of paired redo folds from nested
sparse WAL rotation handoffs that also carry source merge ordinals. This
projection therefore preserves the caller's explicit lineage rather than
reconstructing it: output slots retain restore batch, handoff, merge, fold,
and order coordinates, the exact two-sided checkpoint states, redo-fold
identity, and atomic paired write-ahead/segment-incarnation identity.

Each compacted run must have an inclusive order range and exactly one paired
WAL/segment identity for every represented order. Descending ranges and
truncated or overlong identity vectors return errors with the originating
batch, handoff, merge, fold, and range values. Known and observed checkpoint
IDs are unioned in sorted order. Sparse gaps, absent streams, and absent
checkpoint sides remain absent; the restore does not infer or widen history.

The focused value proofs load `.orna` fixtures with `include_str!` from this
crate and cover overlapping labels across restore batches, handoffs, and
merges, distinct redo folds and WAL rotations, sparse orders, empty catalog
streams, late observations, positionless state, and malformed run rejection.
