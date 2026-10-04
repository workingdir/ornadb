# Storage family 141: paired spill identity across nested restore folds

The family-141 restore projection composes the existing nested paired-spill
restore with an enclosing sequence of restore folds. The outer fold ordinal
is kept in every restored slot; batch, spill, stream, compaction, merge,
source-stream, redo-fold, and order identities are retained at their original
local coordinates. Checkpoint results are grouped in bytewise checkpoint-ID
order, and slots are ordered by their full enclosing coordinate tuple.

The reference does not define this outer spill-fold projection. The pragmatic
choice is therefore to treat container positions as local zero-based
ordinals, keep supplied spill/source/fold/segment identities opaque, and
preserve repeated spill IDs or order numbers as distinct occurrences. Each
present checkpoint generation is pinned to its matching directional segment;
an absent side remains unpinned while its segment pair remains available.
Malformed nested runs retain the existing order-range and segment-count
validation, annotated with the outer fold that failed.

The in-crate fixture proof covers repeated spill identity across folds and
batches, paired and one-sided pins, checkpoint grouping including a catalog-
only checkpoint, and both nested-run validation errors.
