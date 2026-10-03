# Storage family 142: paired compaction identity across nested spill folds

The family-142 projection applies the existing paired-checkpoint nested-spill
restore independently at each outer fold, then groups all resulting orders
under their output checkpoint IDs. Each slot carries the outer fold ordinal
and all inner batch, spill, stream, compaction, merge, nested-stream,
redo-fold, and order coordinates. Directional source checkpoint IDs and
generations remain bound to their matching left/right segment IDs.

The reference does not define this outer fold projection. The pragmatic choice
is to use local zero-based container ordinals, preserve supplied identity
bytes unchanged, retain repeated spill IDs and order labels as separate
occurrences, and keep an absent side unpinned while retaining the segment
pair. Existing invalid-order and segment-count checks are preserved, with the
outer fold ordinal added to each error.

The in-crate fixture proof exercises repeated spill identity across folds and
restore batches, distinct paired source checkpoint pins, a one-sided pin, a
catalog-only checkpoint, and both nested-run validation errors.
