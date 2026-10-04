# Storage family 155: nested restore identity across paired checkpoint compaction folds

Each paired checkpoint compaction fold can carry an independent nested restore
identity pair. Directional bytes are copied unchanged to result streams and
restored slots, including empty catalogue streams. This pair remains separate
from the nested compaction identity and the existing restore-path identity.

Repeated nested restore pairs remain scoped by the restore-fold ordinal. A
malformed checkpoint compaction fold retains its pair on the error while
preserving the family 154 validation error and path.

The in-crate behavior proof loads directional pairs from
`paired-nested-restore-identities-f155.orna` with `include_str!`. It covers
distinct and reused pairs, empty catalogue streams, one-sided source pins,
identity independence, and pair retention on validation failure.
