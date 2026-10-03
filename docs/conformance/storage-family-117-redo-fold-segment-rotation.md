# Storage family 117: redo-fold identity across sparse segment rotations

The read-only storage reference does not specify how paired redo-fold identity
composes with per-frame segment rotation identity. This slice keeps the
identities at their distinct scopes: the directional redo-fold pair belongs to
the enclosing fold, while the directional segment pair belongs to each frame.

- The checkpoint catalog unions known IDs with IDs observed on either side of
  any frame. Every supplied frame projects to every stream, including absent
  checkpoint states.
- Each projected slot retains its fold's exact redo-fold pair and its frame's
  exact segment pair. `(fold_ordinal, order)` distinguishes repeated orders;
  missing orders are not filled in.
- Compaction groups adjacent equal left/right checkpoint states only inside
  one fold and retains the fold pair on the run. Segment rotation does not split
  an otherwise equal state run; its exact per-order directional pairs are
  retained in order on the run.
- Gaps, state changes, and fold boundaries split runs. Missing state remains
  distinct from a present positionless checkpoint, and later observations do
  not erase earlier omissions.

The focused proof parses existing `.orna` fixtures inside the storage crate
through `include_str!` for both redo-fold labels and segment rotations. It
asserts fixture-derived identities and checkpoint values in the sparse stream
and compacted runs.
