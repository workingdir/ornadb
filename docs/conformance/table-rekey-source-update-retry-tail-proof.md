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

`original_owner_key_retries_after_competitor_deletion` covers the matching
release edge: the competitor claims the original owner key, then is deleted so
the owner can retry into that key. Its fixture checks the full ordered mutation
log; this caught-retry ordering remains adapter behavior where the reference is
silent.

`original_owner_retries_after_competitor_nested_closure` covers nested closure
interplay on the final return: the owner first fails to reclaim its original
key, then moves aside to block the competitor's release move. The competitor's
failure closure moves the owner again, retries the competitor move, and lets the
owner take its original key. The fixture documents the resulting mutation log
as local adapter behavior because the reference does not specify this ordering.

`owner_retries_after_competitor_target_delete_closure` covers the paired target
blocker edge: the owner's retry waits while the competitor's alternate target
is occupied, then the competitor's failure closure updates and deletes that
blocker before retrying its move. The owner then reclaims its original key. The
fixture records the local caught-retry order where the reference is silent.

`owner_retries_after_competitor_target_rekey_closure` covers the complementary
release path: the competitor's failure closure updates and re-keys the blocker
aside rather than deleting it, retries its move, and releases the owner's
original key. The complete mutation log records the adapter's local closure
order where the reference is silent.

`owner_retries_after_competitor_closure_reuses_released_target` covers the
follow-on collision: after the blocker moves aside, the owner takes the released
competitor target, so the competitor must enter a second failure closure before
the owner can retry its original key. The fixture asserts the full ordered log;
the reference does not specify this nested caught-retry ordering.

`owner_retries_after_nested_competitor_target_closure` adds a blocker on the
competitor closure's own release move. That closure moves the secondary blocker
aside, releases the competitor target, and then lets the owner retry its original
key. The full log is adapter behavior because the reference is silent on this
nested caught-retry order.

`original_owner_participates_in_nested_competitor_closure` extends that edge
with the owner occupying the secondary blocker's release key. A nested closure
moves the owner aside, releases both competitor moves, and then returns the owner
to its original key. Its complete log documents the local adapter order where
the reference is silent.

`owner_blocks_primary_competitor_closure_retry_then_returns` covers the next
release boundary: after the secondary blocker moves, the owner takes its key and
blocks the primary blocker's retry. The owner moves aside in a nested closure,
allowing both competitor moves to finish before it returns to its original key.
The fixture documents this local mutation order where the reference is silent.

`owner_unblocks_its_nested_move_before_competitor_closure` adds a blocker to the
owner's move-aside target. The owner's nested failure closure moves that blocker
away, then the primary blocker, competitor, and owner complete their retries in
order. The fixture records the local closure log where the reference is silent.

`owner_move_closure_deletes_blocker_in_competitor_tail` covers the paired
release path: the owner's move-aside target is deleted inside its failure
closure, then the primary blocker and competitor finish their retries before
the owner returns. The fixture records the adapter's local caught-retry order
where the reference is silent.

`owner_move_delete_release_target_reuse_needs_nested_retry` covers reuse of the
deleted move target: the primary blocker takes the freed key before the owner's
retry, then moves aside under a nested closure so the owner can move and the
remaining competitor retries can complete. Its ordered log documents local
adapter behavior where the reference is silent.

`owner_retry_after_reused_target_deleted_in_nested_closure` covers the paired
release path: after the primary blocker reuses the deleted owner target, its
nested closure deletes that row, allowing the owner retry to finish before the
competitor returns. The fixture documents this local mutation order where the
reference is silent.

`owner_target_retry_survives_repeated_delete_and_reuse_closures` follows the
owner target through two successive reuses: the primary blocker and then the
secondary blocker each take the freed key and are deleted in nested closures
before the owner retry succeeds. The complete local mutation order is asserted
because the reference is silent on this closure interplay.

`owner_retry_survives_inserted_reuse_after_repeated_target_deletes` extends that
sequence with an inserted row taking the same target after both rekeyed rows
have been deleted. The inserted row is updated and deleted before the owner
retry succeeds; its ordered effects document local adapter behavior where the
reference is silent.

`owner_retry_closure_releases_inserted_reuse_target` keeps the inserted row at
the target when the owner retries. That failed retry is caught; the nested
closure updates and deletes the inserted row before retrying the owner again.
The asserted tail records this local inserted-target closure order where the
reference is silent.

`owner_retry_survives_successive_inserted_target_closures` inserts a second row
at the same target after the first inserted blocker is deleted. The owner fails
and retries through a second nested closure, which updates and deletes the new
blocker before releasing the target. The ordered tail documents this repeated
insert/retry behavior where the reference is silent.

`owner_retry_closure_moves_inserted_reuse_target` covers the move-aside edge in
the second inserted-target closure: the inserted blocker is re-keyed to a free
key and updated there, then the owner's retry claims the vacated target. The
inserted row remains addressable, and the asserted order documents local
closure behavior where the reference is silent.

`inserted_target_move_closure_releases_occupied_destination` covers a blocked
move-aside: another row occupies the inserted blocker's requested key, so a
nested closure updates and deletes that destination blocker before retrying the
inserted re-key. The inserted row then moves and the owner claims its target;
the mutation tail documents this local nested closure order where the reference
is silent.

`inserted_move_retry_survives_reused_occupied_destination` extends that blocked
move closure by inserting a replacement at the just-released move destination.
The inserted row's retry is caught again; that nested closure updates and deletes
the replacement, after which the original inserted row moves and the owner
retry completes. The asserted mutation tail records this local ordering where
the reference is silent.

`competitor_retry_reuses_inserted_move_destination_after_closure` follows the
successful inserted move: a competitor then attempts to claim that destination
and is blocked by the moved row. Its closure moves the inserted row again,
updates it at the new key, and lets the competitor retry before the owner
returns. The local mutation tail captures this nested destination reuse where
the reference is silent.

`competitor_retry_survives_successive_inserted_target_moves` has that competitor
retry against the inserted row's second move destination. A further nested
closure moves the inserted row once more and updates it before the competitor
claims the vacated key and the owner returns. The asserted tail documents this
successive local move and retry order where the reference is silent.

The same fixture and proof then exercise the inserted row reclaiming the
competitor's current key. Its closure updates and moves the competitor aside,
the inserted row retries into the released key, and the competitor reuses the
inserted row's vacated key. This successive moved-target reuse order is a
pragmatic local choice because the reference does not specify it.

The fixture then repeats the handoff in reverse: the inserted row tries the
competitor's reused key, the competitor moves aside, the inserted row retries,
and the competitor takes the newly released key. This repeated direction change
is another local closure choice where the reference is silent.
