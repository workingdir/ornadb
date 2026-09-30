# Table source updates across re-key retry tails

ORNA-MUT-005/006 define atomic `rekey` and its failure cases. They do not specify
the source mutation log produced when a caught re-key failure is followed by
updates to other rows whose own re-keys are still pending.

For the application source-mutation seam, this proof chooses evaluation-order
logging: a failed re-key adds no mutation; successful updates in its recovery
handler remain in the activation; and a later successful re-key of a pending
source carries that row's latest value. This records the adapter's observable
mutation sequence without making it an additional language-level guarantee.

The fixture-backed proofs in `orna-application-v1` cover two final shapes:
`source_updates_to_pending_rows_survive_other_retry_tails` updates sources that
remain pending while later recovery handlers reuse the destination key;
`rekeyed_source_and_reused_source_key_updates_survive_retry_tails` first moves a
source inside a recovery handler, reuses its old key for an inserted row, then
updates both identities while another source retries. Both assert the complete
mutation order and the latest values at their eventual re-keys.

The closure proof,
`moved_source_updates_survive_retry_key_reuse_closure`, adds the return edge: an
inserted replacement leaves the old source key, the moved source reclaims it,
and updates before and after that return stay with the moved identity through
its next re-key. The exact order remains a local adapter proof rather than a
new language-level rule.

`deleted_replacement_key_reuse_survives_retry_closure_edges` covers the related
delete edge: a retry-created replacement is deleted, the moved source reclaims
its original key, and a later retry lets another source reuse that key. It
asserts the complete ordered log, including the deletion and post-reclaim
updates, to make row identity through each key owner change explicit.

`deleting_moved_source_preserves_later_retry_key_owners` covers the final
deletion edge: a moved source is deleted while blocking another retry, then a
replacement and a third source successively own the released keys. The fixture
checks the full log so updates and re-keys remain associated with the current
row identity after each deletion and reuse.
