# Storage family 154: paired checkpoint identity across nested restore compaction folds

Each nested restore compaction fold can carry an independent outer checkpoint
identity pair. Directional bytes are copied unchanged to result streams and
restored slots, including empty catalogue streams. This pair remains separate
from the nested compaction identity and the checkpoint identities already
carried by the restore path.

Repeated outer pairs remain scoped by the restore-fold ordinal. A malformed
nested restore compaction retains its pair on the error while preserving the
family 153 validation error and path.

The in-crate behavior proof loads directional pairs from
`paired-outer-checkpoint-identities-f154.orna` with `include_str!`. It covers
distinct and reused directional pairs, empty catalogue streams, one-sided
source pins, identity independence, and pair retention on validation failure.
