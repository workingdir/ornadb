# Storage family 106: paired redo-chain identity across sparse segment rotation folds

The storage reference specifies paired redo checkpoint values and segment
rotation identity, but does not define how a compacted sparse projection should
represent redo-chain incarnation labels. The v1 projection treats the opaque
left/right chain labels as identity tokens and keeps them with each compacted
run.

- A run extends only across consecutive orders with equal left and right
  checkpoint states **and** equal paired redo-chain identity.
- A change to either chain label splits a run, even when checkpoint state is
  unchanged. Missing entries remain distinct from present generations whose
  position is absent.
- A gap in observed orders splits a run; no missing redo frame is inferred.
- Segment rotation and write-ahead log changes do not split a stable chain.
  The compacted run retains each order's complete paired log/segment identity
  in input order, preserving atomic alignment across sparse folds.
- Both caller-known checkpoint IDs and IDs observed in either side are
  emitted. A known stream omitted from all frames still records the chain and
  gap boundaries.

This is a read-only projection policy. It neither compares chain labels as
ordered values nor rewrites redo state or segment data.
