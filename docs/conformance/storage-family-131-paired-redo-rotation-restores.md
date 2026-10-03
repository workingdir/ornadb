# Storage family 131: paired redo identity across rotation restores

## Behavior

Sparse segment-rotation restores retain the source stream ordinal alongside
the restore batch, handoff, fold, and sparse order coordinates. This matters
when a handoff contains multiple records for the same checkpoint: equal fold
and order labels do not make those records interchangeable. Each restored
slot keeps the original left/right checkpoint values, redo-fold identity, and
directional segment identity from its own source stream. Invalid order ranges
and segment-count mismatches report the stream ordinal that supplied the run.

The checkpoint catalog is the sorted union of known and observed IDs. Restore
order, handoff order, and stream order follow caller input order. Sparse gaps,
omitted streams, and absent checkpoint sides stay absent; restoration does
not synthesize or deduplicate them.

## Reference decision

The reference does not specify this nested restore projection or the identity
of repeated checkpoint streams in one handoff. This implementation treats
source array positions as stable provenance coordinates and retains every
occurrence. That rule is deterministic and avoids collapsing paired redo or
segment lineage when labels overlap.

## Proof fixture

`tests/fixtures/paired-sparse-segment-rotation-stream-ordinals.orna` provides
distinct redo-fold and segment pairs. The focused integration proof submits
two streams with the same checkpoint, fold ordinal, and order, then asserts
their distinct stream ordinals, paired identities, and checkpoint values. It
also checks that malformed input reports the ordinal of the failing stream.
