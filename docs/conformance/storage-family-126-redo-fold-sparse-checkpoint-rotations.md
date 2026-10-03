# Storage family 126: redo-fold identity across sparse checkpoint rotations

The read-only reference does not specify restoration of multiple compacted
checkpoint rotation chains with overlapping local coordinates. The storage v1
policy treats each restore batch and each source handoff as independent
provenance, retaining `(restore_ordinal, handoff_ordinal, fold_ordinal,
order)` for each occurrence.

Each compacted run must contain exactly one directional segment pair per
represented order. Descending ranges and missing or extra pairs are rejected
with the restore, handoff, fold, and order coordinates. Valid runs restore the
exact checkpoint values, redo-fold pair, and segment pair. Known and observed
checkpoint IDs are unioned; omitted streams, absent sides, positionless
generations, and sparse gaps remain unchanged.

The focused proof uses in-crate `.orna` fixtures with `include_str!` and checks
the restored values and identity pairs across two batches, overlapping orders,
rotations, a gap, an omitted stream, and malformed compacted inputs.
