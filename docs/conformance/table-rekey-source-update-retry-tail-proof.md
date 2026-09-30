# Table source updates across re-key retry tails

ORNA-MUT-005/006 define atomic `rekey` and its failure cases. They do not specify
the source mutation log produced when a caught re-key failure is followed by
updates to other rows whose own re-keys are still pending.

For the application source-mutation seam, this proof chooses evaluation-order
logging: a failed re-key adds no mutation; successful updates in its recovery
handler remain in the activation; and a later successful re-key of a pending
source carries that row's latest value. This records the adapter's observable
mutation sequence without making it an additional language-level guarantee.

The fixture-backed proof is
`source_updates_to_pending_rows_survive_other_retry_tails` in
`orna-application-v1`. It updates pending sources from later recovery handlers
while an inserted destination occupant repeatedly leaves and reclaims the same
key, then checks the complete mutation order and final re-key values.
