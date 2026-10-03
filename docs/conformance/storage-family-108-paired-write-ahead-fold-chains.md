# Storage family 108: paired write-ahead identity across sparse stream fold chains

The reference describes checkpoint and write-ahead identities, but does not
specify how multiple sparse checkpoint folds compose when their frame orders
overlap. The v1 projection chains folds in caller order and tags each slot
with its zero-based fold ordinal and original in-fold order.

- The known checkpoint catalog is unioned with IDs observed in every fold, so
  a stream first seen in a later fold is included in the result.
- Each supplied frame contributes one slot for every output stream. Its
  exact checkpoint values and directional `(left_log, right_log)` identity are
  retained, including total stream omissions.
- Equal order numbers from different folds remain separate occurrences
  because `(fold_ordinal, order)` identifies a slot. Order gaps inside a fold
  remain gaps; no absent frame is synthesized.
- Repeated write-ahead pairs remain present per occurrence and are never
  deduplicated across folds.

This is a read-only composition policy. It does not merge checkpoint values
or reinterpret opaque write-ahead IDs.
