# Storage family 124: redo-fold identity across sparse restore handoffs

The read-only reference is silent on combining multiple restored compaction
handoffs whose local ordinals overlap. The v1 policy treats each input restore
batch as independent provenance and tags every restored occurrence with its
batch ordinal. The original handoff ordinal, fold ordinal, order, directional
redo-fold identity, and exact checkpoint values are retained as separate
coordinates and values.

The known and observed checkpoint catalog is unioned, but absent streams and
sparse orders are not synthesized across restore batches. Positionless
checkpoint generations remain present. Invalid compacted ranges return an
error carrying both the restore-batch and handoff/fold coordinates that failed.

The focused proof builds two in-crate `.orna` fixture-backed compacted handoff
batches with overlapping local coordinates and reused fold identity. It
asserts the expanded values and provenance tuples, proves an omitted stream
stays absent, and checks that a malformed range identifies the failing restore
batch.
