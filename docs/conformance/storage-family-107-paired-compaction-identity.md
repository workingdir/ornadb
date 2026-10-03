# Storage family 107: paired compaction identity across sparse redo-chain rotations

The reference defines compaction jobs and their IDs, but is silent on how
paired compaction IDs should be represented by a sparse redo-chain projection.
The v1 policy treats the left/right IDs as an opaque per-order pair and keeps
that pair alongside the paired log/segment lineage.

- Runs join only consecutive orders with equal full checkpoint state on both
  sides and equal paired redo-chain identity.
- Compaction ID changes do not split a stable redo-chain run. Each original
  left/right compaction pair is retained once per input order, in order, so
  compaction history is not collapsed or cross-paired during segment rotation.
- A redo-chain change or missing order starts a new run. No missing checkpoint
  state is inferred; absent and present positionless generations remain
  distinct.
- Caller-known checkpoint IDs and IDs observed in either side are both
  emitted, including streams omitted from all input frames.

This is a read-only v1 projection policy. It does not compare compaction IDs as
ordered values or modify checkpoint, compaction, log, or segment records.
