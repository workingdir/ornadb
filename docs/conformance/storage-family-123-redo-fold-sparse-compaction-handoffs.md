# Storage family 123: redo-fold identity across sparse compaction handoffs

The read-only reference is silent on handing off independently compacted
sparse redo-fold chains. The v1 policy copies each source run and binds it to
the source handoff ordinal while retaining its local fold ordinal and order
range. It unions known and observed checkpoint IDs but does not merge runs
across handoff boundaries or synthesize absent checkpoint values.

Restoration expands only the orders represented by each run. The handoff and
fold ordinals, paired fold identity, and exact left/right checkpoint values
survive expansion; gaps remain absent. A descending order range is rejected
with its handoff and fold coordinates, rather than silently producing an empty
history.

The focused proof uses in-crate `.orna` fixtures through `include_str!` to
exercise two compacted sources with a shared fold identity, overlapping local
coordinates, sparse gaps, a checkpoint stream omitted by one source, a
late-observed stream, and a catalog-only checkpoint. It checks the merged runs,
restored values and identities, and malformed-range rejection.
