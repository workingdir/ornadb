# Storage family 115: redo-chain fold identity across write-ahead compaction omissions

The reference does not define how an absent write-ahead compaction observation
affects paired redo-chain identity across sparse checkpoint folds. The v1
policy keeps redo-chain and log/segment provenance on every supplied frame and
represents each compaction observation as an independent optional pair.

- The stream catalog unions caller-known checkpoint IDs with IDs observed in
  any fold. Every supplied frame contributes a slot to every stream, including
  checkpoint omissions.
- A compacted run joins adjacent orders in one fold only when both checkpoint
  states and the paired redo-chain identity are equal. A missing or changed
  compaction observation, or a log/segment rotation, does not split the run.
- Each run retains one log/segment pair and one optional compaction pair for
  every represented order. `None` means the frame had no compaction
  observation; it does not erase the redo-chain label or invent a pair.
- Fold boundaries, state changes, redo-chain changes, and sparse order gaps
  split runs. Equal order numbers in separate folds remain distinct.
- Missing checkpoint state remains distinct from a present positionless
  checkpoint. Later-observed streams retain their earlier omission slots.

The behavior proof loads actual paired chain, write-ahead, segment, compaction,
and checkpoint values from `.orna` fixtures inside the storage crate via
`include_str!`.
