# Storage family 140: nested compaction identity across paired checkpoint restore folds

The 1.0.0 reference does not define the composition of paired checkpoint
restore folds with nested compacted segment-rotation streams. This slice uses
container positions as the stable identities for that silent case.

The restore input is nested as restore fold, restore batch, handoff, and
checkpoint stream; each stream carries compacted runs. The resulting occurrence
identity is the tuple of outer restore-fold ordinal, restore ordinal, handoff
ordinal, stream ordinal, compaction ordinal, source merge ordinal, source redo
fold ordinal, and order. Ordinals are zero-based and local to their parent.
Equal checkpoint IDs, fold labels, and order values in separate nested
occurrences are retained rather than deduplicated.

Each expanded slot retains its directional redo-fold identity and exact paired
segment identity. A present checkpoint generation pins only its same-side
segment; an omitted side stays unpinned. Known checkpoint IDs remain in the
result even when no stream supplies a slot. Invalid order ranges and segment
identity cardinalities remain errors and are wrapped with the outer restore
fold ordinal that contained the malformed run.
