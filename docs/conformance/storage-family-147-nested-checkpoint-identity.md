# Storage family 147: nested checkpoint identity across paired restore spill compaction folds

Each paired restore spill compaction fold can carry an opaque nested
checkpoint identity pair. The pair is copied unchanged to the result streams
and every restored slot. It remains independent from the enclosing checkpoint
identity pair, paired restore identity, spill identity, scalar restore-fold
label, output checkpoint key, and optional source pins.

Empty catalogue streams retain the fold's nested checkpoint pair. A malformed
nested compaction retains the same pair while preserving the family 146 restore
error unchanged, including its nested validation path. Directional bytes are
not interpreted or normalized; the reference does not define this extra
identity layer.

The in-crate behavior proof loads the left/right checkpoint pairs from
`paired-nested-checkpoint-identities-f147.orna` with `include_str!`. It covers
reused and distinct pairs across restore folds, independence from the existing
checkpoint and restore identities, empty catalogue results, source pin
preservation, and identity retention on validation failure.
