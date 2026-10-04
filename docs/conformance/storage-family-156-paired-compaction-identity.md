# Storage family 156: paired compaction identity across nested restore checkpoint folds

Each nested restore checkpoint fold can carry an independent paired
compaction identity. Directional bytes are copied unchanged to result streams
and restored slots, including empty catalogue streams. This pair remains
separate from the nested restore and nested compaction identities already
carried by the fold.

Repeated compaction pairs remain scoped by the restore-fold ordinal. A
malformed checkpoint fold retains its pair on the error while preserving the
family 155 validation error and path.

The in-crate behavior proof loads directional pairs from
`paired-compaction-identities-f156.orna` with `include_str!`. It covers distinct
and reused pairs, empty catalogue streams, one-sided source pins, identity
independence, and pair retention on validation failure.
