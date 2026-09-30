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

`returned_key_competitor_return_then_final_owner_delete_closes_retry` adds the
follow-on owner cycle: the competitor returns to its vacated source key before
the final source claims the returned key, then retries that key and deletes the
new owner before claiming it. The fixture proves the ordered ownership changes;
the reference remains silent on this caught-retry ordering.

`returned_key_competitor_retry_after_final_owner_rekeys_away` covers the paired
release path: when the final owner re-keys away instead, the competitor's retry
claims the contested key and the prior owner returns to its original source
key. The fixture checks every update and owner transition in order, using the
same evaluation-order choice where the reference is silent.

`returned_key_owner_return_retry_after_competitor_rekeys_into_source_key`
covers the next return conflict: the competitor claims the final owner's
vacated source key, blocking its return re-key; when the competitor moves aside,
the owner retries successfully. The fixture checks the collision, release, and
retry sequence against evaluation order because the reference does not specify
this caught-retry ordering.

`owner_retry_waits_for_competitor_target_closure` adds a nested block on the
competitor's move target. The fixture records the target owner's update and
deletion, the competitor's successful move, and the final owner's retry, all in
evaluation order. ORNA-MUT-005/006 do not define this caught-retry mutation log.

`competitor_and_owner_retries_alternate_returned_key_closure` continues from
that closure: the competitor retries into the returned key, the owner moves
aside, and the competitor succeeds; then the owner retries after the competitor
moves away again. The fixture asserts every owner transition and attached
update. The reference is silent on the resulting caught-retry log order.

`original_owner_key_retries_after_competitor_closure` follows the original
owner back to its first key after the alternating cycle. It records the
competitor taking that key, the owner moving aside, and both rows taking turns
retrying into the released key. This full log uses the same adapter evaluation
order because the reference does not specify caught-retry mutation sequencing.

`original_owner_key_and_competitor_alternate_final_retry` isolates the final
two-row snapshot from that cycle: the competitor takes the original owner key,
the owner moves aside and retries, then the competitor moves back and the owner
retries again. Its complete ten-mutation log documents the local evaluation
order without adding a language-level guarantee.
