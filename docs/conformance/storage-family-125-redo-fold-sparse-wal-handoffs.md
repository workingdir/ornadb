# Storage family 125: redo-fold identity across sparse WAL restore handoffs

The read-only reference does not define how multiple compacted WAL handoffs
from independent restore batches combine. The storage v1 policy keeps each
batch and each source handoff as separate provenance coordinates, yielding
`(restore_ordinal, handoff_ordinal, fold_ordinal, order)` for every restored
occurrence.

Each compacted run must contain exactly one atomic paired write-ahead
log/segment identity per represented order. A descending range or a missing
or extra identity is rejected with all provenance coordinates. Valid runs
expand in ascending order; fold identity, checkpoint sides, and each exact
log/segment pair are copied unchanged. The known and observed checkpoint
catalog is unioned, while absent streams, missing sides, positionless
generations, and sparse gaps stay absent or unchanged.

The focused proof uses in-crate `.orna` fixtures and asserts real restored
checkpoint generations, fold pairs, WAL/segment pairs, repeated coordinates
across batches, sparse gaps, catalog-only streams, and malformed run errors.
