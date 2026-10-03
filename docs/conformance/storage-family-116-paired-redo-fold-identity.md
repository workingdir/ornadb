# Storage family 116: paired redo-fold identity across sparse checkpoint chains

The storage reference does not prescribe how a directional identity for a
redo fold composes with sparse checkpoint frames. The v1 policy treats that
pair as opaque fold provenance and keeps it separate from the fold ordinal and
the checkpoint values.

- The stream catalog unions caller-known checkpoint IDs with IDs observed in
  any fold. Every frame in each fold contributes one slot to every stream,
  including checkpoint omissions.
- Each slot carries the fold's exact directional identity pair. The pair is
  repeated for each supplied order; it is not deduplicated across frames.
- Compaction joins only adjacent orders within one fold when both complete
  checkpoint generations and the paired fold identity are equal. Fold
  boundaries and sparse gaps remain distinct even when the pair is reused.
- Missing checkpoint values remain distinct from present positionless values;
  streams first observed later retain their earlier omission slots.
- `(fold_ordinal, order)` remains the occurrence key. A repeated fold pair or
  order does not collapse separate occurrences.

The focused behavior proof reads the paired fold labels and checkpoint states
from `.orna` fixtures inside the storage crate via `include_str!`.
