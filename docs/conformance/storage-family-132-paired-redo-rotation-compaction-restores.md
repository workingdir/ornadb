# Storage family 132: compaction lineage in paired redo rotation restores

## Behavior

Sparse checkpoint rotation restore slots retain the full source coordinate
tuple `(restore_ordinal, handoff_ordinal, stream_ordinal,
compaction_ordinal, fold_ordinal, order)`. In particular, two compacted runs
from the same source stream remain distinct when their fold and sparse order
labels overlap. Each expanded order carries the exact left/right checkpoint
values, paired redo-fold identity, and directional segment incarnation from
its source run.

Invalid ranges and segment identity count mismatches report the same source
coordinates, including the compaction run ordinal. Checkpoint streams are the
sorted union of known and observed IDs; sparse gaps, omitted streams, and
absent sides are not synthesized.

## Reference decision

The reference does not specify compaction-run identity in this nested sparse
checkpoint rotation restore projection. Caller order is the stable provenance
rule, so the implementation exposes the source run's array position rather
than inferring identity from fold or order labels.

## Proof fixture

`tests/fixtures/paired-sparse-checkpoint-rotation-compaction-ordinals.orna`
contains distinct paired redo-fold and segment identities. The focused proof
uses two runs with identical fold and order labels, checks their different
compaction ordinals and exact restored values, and verifies malformed input
reports the failing run's ordinal.
