# Storage family 127: redo-fold identity across sparse segment-rotation restores

The read-only reference does not define restoring several compacted merge
chains as independent segment-rotation handoffs. The storage v1 policy retains
`(restore_ordinal, handoff_ordinal, merge_ordinal, fold_ordinal, order)` for
each occurrence. The original merge coordinate remains distinct from the
restore batch and handoff that contain it.

Every compacted run must carry exactly one directional segment pair for each
represented order. Descending ranges and missing or extra pairs are rejected
with all provenance coordinates. Valid runs copy checkpoint generations,
redo-fold identity, and segment identity exactly. Known and observed
checkpoint streams are unioned; gaps, absent sides, and streams not present
in a handoff are not synthesized.

The focused proof uses in-crate `.orna` fixtures through `include_str!` and
asserts values, identity pairs, overlapping coordinates, sparse omissions, and
both malformed-run errors.
