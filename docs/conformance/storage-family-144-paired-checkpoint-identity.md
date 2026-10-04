# Storage family 144: paired checkpoint identity across nested restore spill folds

Each outer nested-spill restore fold may carry a caller-supplied ordered pair
of checkpoint identity bytes. The pair is copied to every returned checkpoint
stream and slot, in addition to the existing output checkpoint ID, restore-fold
identity, and directional source pins. A pair remains available on catalogue
entries with no slots and on slots where one or both source pins are absent.
Repeated pairs in separate folds remain separate because the restore-fold
ordinal is retained.

The reference does not define this extra identity layer. The pragmatic choice
is to treat both byte strings as opaque, preserve the left/right direction
exactly, accept equal or empty sides, and never derive the pair from a source
pin or output checkpoint ID. Existing nested restore validation is unchanged;
errors retain their restore-fold path and are additionally tagged with the
caller-supplied pair.

The in-crate fixture proof asserts distinct directional pairs on repeated
checkpoint results, pair retention for an empty catalogue entry, one-sided
pin behavior, and paired identity on a nested validation error.
