# Storage family 151: nested spill identity across paired compaction restore folds

Each paired compaction restore fold can carry an independent nested spill
identity pair. Directional bytes are copied unchanged to every result stream
and restored slot, including empty catalogue streams. This fold-scoped pair
remains separate from each nested spill's own identity and from the outer
checkpoint, nested restore, outer spill, nested checkpoint, and restore
checkpoint identity pairs.

Repeated pairs across folds remain scoped by the restore-fold ordinal. A
malformed nested compaction retains the fold-scoped pair on its error while
preserving the family 150 validation error and path.

The in-crate behavior proof loads identity pairs from
`paired-nested-spill-identities-f151.orna` with `include_str!`. It covers reused
and distinct pairs, empty catalogue streams, one-sided source pins, separation
from per-spill identities, and identity retention on validation failure.
