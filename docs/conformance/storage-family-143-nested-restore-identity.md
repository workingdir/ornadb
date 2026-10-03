# Storage family 143: nested restore identity across paired compaction spills

Family 143 scopes each paired-checkpoint spill restore under a caller-supplied
opaque restore-fold identity. Each fold owns its checkpoint catalogue and
produces its own checkpoint streams in sorted checkpoint-ID order; repeated
checkpoint IDs across folds are not combined. Every restored order retains
the fold's caller ordinal and identity alongside its inner restore, spill,
stream, compaction, merge, nested-stream, redo-fold, and order coordinates.
The two source checkpoint pins retain their own checkpoint IDs and
generations and bind to the matching directional segment.

The reference is silent on this explicit restore-fold identity layer. The
pragmatic choice is to preserve identity bytes as opaque, keep container
positions as local zero-based ordinals, and retain repeated spill IDs or order
labels as distinct occurrences. Missing source sides stay unpinned while the
segment pair remains present. Existing order-range and segment-count
validation is unchanged; errors are additionally tagged with the failing
outer fold identity and ordinal.

The in-crate fixture proof covers repeated checkpoint/spill identities in
separate folds and batches, fold-local empty catalogue entries, paired and
one-sided source pins, and both validation errors with their full nested
coordinates.
