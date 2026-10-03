# Storage family 122: redo-fold identity across sparse checkpoint merge chains

The read-only reference is silent on merging already-folded sparse checkpoint
chains. The v1 policy treats each input chain as independent provenance: it
copies every represented occurrence and tags it with that chain's input
ordinal. Local `(fold_ordinal, order)` coordinates therefore remain distinct
when they overlap across chains. The merge unions known and observed
checkpoint IDs, retains paired fold identities and exact left/right values,
and sorts occurrences by source chain, fold, then order.

The merge does not synthesize absent streams, frames, or checkpoint states.
Catalog-only streams have no slots when no source represented that ID; streams
present in one chain are not widened into other chains. Checkpoint omissions
inside represented slots stay `None`, and positionless generations remain
present values.

The focused proof uses in-crate `.orna` fixtures through `include_str!` to
construct two source chains with overlapping orders, a reused fold identity,
different checkpoint values at the same local coordinate, a sparse gap, an
omitted checkpoint stream, and a late-observed stream. It checks the full
merged occurrence coordinates and the computed checkpoint and identity values.
