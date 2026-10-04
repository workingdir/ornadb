# Storage family 150: paired checkpoint identity across nested compaction restore folds

Each nested compaction restore fold can carry an independent outer checkpoint
identity pair. The directional bytes are copied unchanged to every output
stream and restored slot, including empty catalogue streams. This outer pair
remains distinct from the existing checkpoint pair, nested checkpoint pair,
nested restore pair, outer spill pair, and each nested spill's own identity.

Repeated outer checkpoint pairs across folds remain scoped by the restore-fold
ordinal. A malformed nested compaction retains the pair on its error while
preserving the family 149 validation error and path.

The in-crate behavior proof loads outer checkpoint pairs from
`paired-outer-checkpoint-identities-f150.orna` with `include_str!`. It covers
reused and distinct directional pairs, empty catalogue streams, nested spill
compaction output, one-sided checkpoint pins, and identity tagging on
validation failure.
