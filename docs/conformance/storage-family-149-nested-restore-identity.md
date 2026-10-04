# Storage family 149: nested restore identity across paired spill compaction checkpoint folds

Each paired spill compaction checkpoint fold can retain an independent nested
restore identity pair. Directional bytes are copied unchanged to output
streams and restored slots, including empty catalogue streams. This nested pair
is separate from the existing restore pair, nested checkpoint pair, outer
spill pair, each nested spill's own pair, the scalar restore-fold label, and
optional checkpoint pins.

Repeated nested restore pairs across folds remain scoped by the output
restore-fold ordinal. A malformed nested compaction retains the pair on its
error while preserving the family 148 validation error and path.

The in-crate behavior proof loads identity pairs from
`paired-nested-restore-identities-f149.orna` with `include_str!`. It covers
reused and distinct pairs across folds, identity independence, empty catalogue
streams, nested spill identity and pin retention, and identity tagging on
validation failure.
