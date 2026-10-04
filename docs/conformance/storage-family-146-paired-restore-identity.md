# Storage family 146: paired restore identity across nested spill compaction folds

Nested paired-checkpoint spill restore folds can carry an independent opaque
restore identity pair. The left and right bytes identify restore incarnations;
the implementation copies them unchanged and does not interpret or normalize
them.

The restore identity is independent of the scalar restore-fold label, paired
checkpoint identity, nested spill identity, output checkpoint key, source pins,
and numeric fold ordinal. Each output stream and every restored slot retains the
pair. Empty catalogue streams also retain it. On validation failure, the error
includes the failed fold's restore pair and the nested error continues to carry
the checkpoint pair, spill pair, and validation coordinates.

The in-crate behavior proof loads directional identities from
`paired-restore-identities-f146.orna` with `include_str!`. It exercises repeated
restore identity across separate folds, a retry with different directional
bytes under the same scalar fold label, repeated spill labels across batches,
catalogue-only streams, one-sided pins, preserved compaction coordinates, and
identity tagging on a malformed segment-identity count.
