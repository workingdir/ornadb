# Storage family 129: sparse compaction rotation restores

The reference does not define how nested restore batches and handoffs retain
the source positions of repeated checkpoint streams and compacted runs. This
projection uses input positions as stable ordinals and returns them with each
restored slot: restore batch, handoff, source stream, compaction run, fold, and
order. It also preserves the exact two-sided checkpoint values, redo-fold
identity, and directional segment-incarnation pair.

Each represented order must consume exactly one segment pair. Descending
ranges and identity-count mismatches return errors carrying all source
ordinals and the original range. The known and observed checkpoint IDs are
unioned in sorted order; sparse gaps, duplicate source streams, catalog-only
streams, and absent checkpoint sides are retained as supplied without
synthesizing state.

The focused proof uses an in-crate `.orna` fixture through `include_str!` and
asserts actual restored coordinates, checkpoint values, fold identities,
segment pairs, and malformed-run errors.
