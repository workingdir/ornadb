# Storage family 148: paired spill identity across nested checkpoint restore compaction folds

Each nested checkpoint restore compaction fold can carry an independent outer
spill identity pair. The directional bytes are copied unchanged to every
result stream and restored slot, including empty catalogue streams. The pair
remains separate from the nested checkpoint identity, enclosing checkpoint
and restore identities, the scalar restore-fold label, and each nested spill's
own spill identity.

Reusing an outer pair across restore folds does not combine their results:
streams retain their restore-fold ordinal. A malformed nested compaction keeps
the outer pair on its error while preserving the family 147 validation error
and its path.

The in-crate behavior proof loads outer spill pairs from
`paired-outer-spill-identities-f148.orna` with `include_str!`. It covers
reused and distinct pairs across folds, independence from nested spill pairs,
empty catalogue streams, source pin retention, and outer identity tagging on
validation failure.
