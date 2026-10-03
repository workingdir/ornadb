# Storage family 130: checkpoint compaction handoff restores

The reference does not specify how repeated checkpoint stream entries or
compacted run positions survive nested sparse restore batches. This restore
projection treats those input positions as stable source coordinates and
retains restore batch, source stream, compacted run, original handoff, fold,
and order on each result. The exact two-sided checkpoint values and paired
redo-fold identity are copied from the source run.

Known and observed checkpoint IDs are unioned in sorted order. Descending
order ranges are rejected with the complete source path. Sparse gaps, empty
streams, duplicate checkpoint stream entries, and omitted checkpoint sides
are preserved as supplied; restore does not synthesize history. The focused
proof loads an in-crate `.orna` fixture through `include_str!` and verifies
overlapping fold/order labels remain distinguishable by their source stream
and run positions.
