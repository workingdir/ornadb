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

`returned_source_delete_and_retry_reuse_key_in_order` closes the last sequence:
the moved source returns to its original key, is deleted there, and that key is
reused by a replacement before another retry deletes the replacement and lets
the original destination row claim it. The fixture asserts every update,
deletion, and re-key in order.

`returned_source_delete_reuse_by_competitor_allows_final_retry` covers the
remaining owner edge: after that returned source is deleted, a pending source
claims its key, blocks another retry, and then moves aside so the last source
can claim it. The complete mutation log records each owner transition and the
updates attached to those owners. ORNA-MUT-005/006 are silent on this competition
between caught retries, so this proof follows evaluation order: the successful
claim owns the freed key until its later re-key. This remains adapter behavior,
not a new language-level guarantee.

`returned_source_delete_competitor_deletion_allows_final_retry` covers the
matching delete edge: the source that claims the returned key is deleted while
blocking the final retry, after which the retry takes the freed key. Its fixture
asserts the complete log, including the competitor deletion and final source
re-key under the same evaluation-order choice.
