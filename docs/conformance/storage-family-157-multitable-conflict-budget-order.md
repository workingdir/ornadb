# Merge family 157: multi-table row and checkpoint conflict budget order

ORNA-MERGE requires typed row and checkpoint conflicts but does not prescribe
their cross-phase traversal order. The storage merge policy plans row conflicts
first in stable table/key order, then visits checkpoint IDs in byte order. This
proof extends the shared-budget case across multiple tables so a table-order
change cannot silently reorder bounded conflict evidence.

The fixture-backed test covers both side orientations for two table conflicts
and a reset-versus-advance checkpoint conflict. It checks the conflict lower
bound and affected ranges/checkpoints below budget, the exact ordered conflict
list at the closing budget, and that clean tombstones and reset closures remain
outside conflict details. A zero conflict budget stops after the first
conflicting table; larger budgets load only through the table that crosses the
budget before the checkpoint phase. The fixture inputs are loaded from this
crate using `include_str!`.
