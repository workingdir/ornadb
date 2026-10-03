# Storage family 92: depth path identities across restore omissions

MERGE-1 does not define a dense path-identity view for paired column restore
storms. The v1 `column_restore_storm_depth_path_slots()` extension uses the
committed storm folds and preserves these distinctions:

- Each slot carries its stable `(table, column)` pair, storm index and order
  range, and wave order.
- An omitted column is represented by `fragments: None`, including when that
  column is absent for an entire storm.
- A present labeled empty fragment remains present with its local label and
  an empty `snapshot_paths` list. Its identity is not borrowed by a later
  wave.
- Paths are the canonical row keys from the exact fragment. Labels restart
  per wave and remain local across omissions and storm boundaries.

The fixture proof covers a column present on either side of an omitted wave,
including an empty labeled fragment, and checks the exact paths after it
returns.
