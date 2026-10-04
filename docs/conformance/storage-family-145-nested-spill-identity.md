# Storage family 145: nested spill identity across paired restore checkpoint folds

Each nested spill in a paired-checkpoint restore fold may carry an opaque,
directional spill identity pair. The pair is recovered by its restore-fold,
batch, and spill positions and copied to every restored slot. It stays
independent of the shared scalar spill label, output checkpoint ID, enclosing
checkpoint identity pair, and optional source pins. Repeated scalar spill
labels therefore retain distinct spill incarnations across batches and folds.

The reference does not define this additional identity layer. The pragmatic
choice is to preserve caller bytes without interpreting or normalizing them,
retain left/right direction, and tag validation errors with both the enclosing
checkpoint pair and the affected spill pair. Empty catalogue streams retain
their checkpoint pair even when no spill slot exists.

The in-crate fixture proof covers repeated scalar spill labels with distinct
paired identities, a repeated identity at a later batch position, separate
checkpoint pairs across restore folds, empty catalogue entries, one-sided
source pins, and identity retention on a nested validation error.
