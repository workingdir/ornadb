# Storage family 153: nested compaction identity across paired restore checkpoint folds

Each paired restore checkpoint fold can carry an independent nested
compaction identity pair. Directional bytes are copied unchanged to result
streams and restored slots, including empty catalogue streams. This pair is
separate from run compaction ordinals and the existing restore, spill, and
checkpoint identity pairs.

Repeated pairs across folds remain scoped by the restore-fold ordinal. A
malformed nested compaction retains its pair on the error while preserving the
family 152 validation error and path.

The in-crate behavior proof loads identity pairs from
`paired-nested-compaction-identities-f153.orna` with `include_str!`. It covers
reused and distinct pairs, empty catalogue streams, one-sided source pins,
ordinal scoping, and identity retention on validation failure.
