# Storage family 152: paired restore identity across nested compaction checkpoint folds

Each nested compaction checkpoint fold can carry an independent outer restore
identity pair. Directional bytes are copied unchanged to every result stream
and restored slot, including empty catalogue streams. This pair remains
independent of the existing restore pair, nested restore pair, nested spill
pair, outer checkpoint pair, and the compaction spill's own identity.

Repeated pairs across folds remain scoped by the restore-fold ordinal. A
malformed nested compaction retains the outer pair on its error while
preserving the family 151 validation error and path.

The in-crate behavior proof loads directional pairs from
`paired-outer-restore-identities-f152.orna` with `include_str!`. It covers
reused and distinct pairs, empty catalogue streams, one-sided source pins, pair
independence, and identity retention on validation failure.
