# 16. Administrative operations {#administration}

## Administrative boundary

Administrative functions operate on repository, storage or execution state that ordinary table mutation cannot reach. They are not methods on mutable system rows. [The API reference](#system-reference) gives each exact signature; this chapter supplies the corresponding transition contract.

A call is admitted only through the trusted local process boundary or an authenticated endpoint explicitly permitting administration. Possessing a row reference or knowing a plan token is not additional authority. An application evaluation endpoint does not acquire administrative permission merely because it accepts Orna expressions.

**ORNA-ADMIN-001** Every administrative call MUST record an invocation with the function, safe arguments, owner, observed generation and terminal outcome. State-changing calls MUST retain an audit event in the same local transaction as their durable transition, or in the recoverable journal for a Git transition. A failed call records failure without claiming that its requested state change occurred.

**ORNA-ADMIN-002** Administrative operations MUST NOT run reentrantly inside an activation holding uncommitted table writes. Such a call fails with `sys.admin.busy`; the caller may finish its transaction and submit administration as a new activation. Read-only planning and verification may inspect a pinned generation without mutating it.

Every argument is resolved and type-checked before mutation. A reference from another runtime fails when it denotes a live object. A snapshot reference remains pinned. Missing data is hydrated only under the configured availability policy. Failure to obtain required objects leaves the prior authoritative generation unchanged.

### Shared preconditions and failure classes

All mutating operations require a writable attachment and fail with `sys.admin.read_only` otherwise. All reject invalid argument types through the ordinary type/argument diagnostics. `sys.admin.busy` identifies an incompatible active owner/lease or a reentrant write activation. Invalid candidate invariants produce `sys.admin.assertion_failed`. Missing objects produce `sys.admin.missing_object` or the more specific snapshot/source/storage code before mutation. A cancellation before durable admission has no transition; after admission the operation follows its journal or transaction to a safe boundary and reports whether it completed or was cancelled.

A successful no-op returns the documented current value or `false`, not an invented new generation. Unexpected I/O failure is an ordinary failure with a retained safe cause and a recovery record if admission already occurred. It must not be translated into success. State-dependent conditions are rechecked under the lock or compare-and-set transaction; checking them only when a preview is generated is insufficient.

## Repository operations

| Operation | Required state and target | Atomic boundary, audit and failure behaviour |
|---|---|---|
| `plan_checkout` | Resolvable committed target; no mutation permission required to inspect an otherwise accessible target. | Pins HEAD, CWD, index, worktree and pending generation into `sys.CheckoutPlan`. Records a read invocation, not a checkout event. Does not pause consumers, create objects or switch HEAD. Missing or invalid target fails without changes. |
| `create_branch` | Valid non-existing local branch name; supplied committed start point or current committed HEAD. | Git compare-and-set from absent to target commit, with a local journal/audit event. Does not switch, stage, commit, pause streams or alter pending changes. `sys.git.unborn_head`, `sys.git.uncommitted_snapshot`, `sys.git.invalid_ref` or `sys.git.branch_exists` leave all existing refs and CWD untouched. |
| `checkout` | Resolvable committed target; safe carry-forward, or exact authorised destructive plan; valid target schema and assertions. | [CHECKOUT-1](#branching). Records target attachment and preserved/discarded sets. Dirty conflict gives `sys.git.dirty_conflict`; a stale token gives `sys.git.stale_plan`. Neither admits a partial switch. Force cannot bypass invariants. |
| `commit` | Valid staged state, nonempty staged delta, safe author identity and message; no conflicting publication lease. | Builds the staged candidate, validates it, creates its commit, then uses the [publication reconciliation protocol](#publication) to preserve unstaged work. Records commit/ref transition and exact staged generation. An unchanged index gives `sys.git.nothing_to_commit`; stale state is re-planned or rejected before ref change. It never consumes a newer unstaged tail. |

`sys.admin.commit` has no implicit “allow empty” option. The Git-compatible CLI may expose Git's explicit option separately. Default author information comes from configured Git identity; missing identity fails before creating a commit. The committer and commit timestamp are recorded independently of the authored identity and timestamp.

### Unborn branches

`create_branch` is equivalent to creating a ref at an existing commit, so an absent default HEAD commit fails. `orna switch -c name` in an unborn worktree follows Git's distinct behaviour: it selects the new unborn symbolic branch and preserves staged and unstaged content; no branch ref points to a fabricated commit. The first actual commit creates that branch ref. The operation is journalled like other symbolic-HEAD changes and does not reset allocators or checkpoints.

## Storage operations

| Operation | Preconditions | Atomic boundary and result |
|---|---|---|
| `flush` | Optional table exists and is writable; frozen pending generation selected. | Seal that generation durably into storage objects and update the frozen local root metadata. Do not move a Git ref or consume later writes. Return exact rows/bytes/storage nodes sealed. An empty tail is a successful zero result. Physical failure leaves old data authoritative and reports unavailable/corrupt storage as appropriate. |
| `compact` | Selected indexed storage roots exist, are verified and do not overlap an incompatible maintenance lease. | Build replacements off to the side, compare the input generation, then atomically admit the local canonical root. Preserve semantic rows, keys and checkpoints. Record a compaction job. Stale inputs give `sys.storage.rewrite_conflict`; cancellation discards unadmitted outputs. |
| `verify` | Readable selected scope; a pinned observation generation. | Inspect without repair. Return all discovered errors/warnings in `sys.VerificationReport`; the report distinguishes corruption from unavailable promised bytes. A failed integrity check is report data, not an implicit repair. Failure to complete the scan is `sys.admin.verification_failed` with a partial safe diagnostic, never a clean report. |

Objects written before failed compaction admission are unreferenced temporary objects and may be garbage-collected after recovery. A reference to old pinned data continues to resolve for its retained lifetime; switching the active root does not rewrite the meaning of that reference. No maintenance operation advances a source checkpoint.

## Runs, streams and delivery state

| Operation | Preconditions | Transition and outcome |
|---|---|---|
| `cancel_run` | Run reference resolves in the current owner context. | Request cancellation, stop admission of new items, cancel and join descendants, roll back open transactions, then mark the run terminal. Return `true` only if this call first requested cancellation; an already terminal/cancelling run returns `false`. Previously committed items remain. |
| `pause_stream` | Stream is live and belongs to the current owner; caller does not hold its callback transaction. | Stop new item admission and wait for the current item/batch boundary. Mark paused and record the reason. Return `false` if already paused/terminal; do not cancel unrelated streams. Cancellation of the pause request leaves either the prior state or an acknowledged paused state, explicitly reported. |
| `resume_stream` | Stream is paused, its run is live, source is available, consumer identity is unchanged and no incompatible checkpoint reset or blocking failure is unresolved. | Acquire admission lease and mark running atomically. Return `false` if already running. Failure leaves it paused; it never guesses a source position or clears a failure. |
| `reset_checkpoint` | Source supports the requested position; stream paused and no in-flight delivery; expected version and position match; no unresolved blocking failure. | Compare-and-set position and version plus an audit `sys.CheckpointUpdate` in one transaction. Preserve provider format and full consumer identity. Stale state gives `sys.checkpoint.conflict`, unsupported position gives `sys.checkpoint.not_replayable`. No callback is executed. |
| `retry_failure` | Same stable delivery, expected failure version/status, current checkpoint still at its recorded predecessor, replayable payload/source and no attempt lease. | Lease the delivery, increment attempt count/version, create a new invocation and execute its callback. Follow [DELIVERY-1](#checkpoints). Failed retry returns the same failure identity to open, without advancing progress. |
| `skip_failure` | Blocking open delivery, matching failure version/status and checkpoint predecessor; provider supplies a safe successor; reason present. | Move the checkpoint, mark the same failure skipped and write an audit record in one transaction. No user callback runs. Unsupported skip gives `sys.failure.skip_unsupported`; stale failure gives `sys.failure.stale_version`. |
| `replay_failure` | Preserved nonblocking skipped delivery, matching failure version/status, available safe payload/source and no replay lease. | Execute separately; do not rewind or advance the live checkpoint. Success marks replayed; ordinary failure returns to skipped with updated diagnostic/version. Return a handle owned under the normal task rules. |
| `resolve_failure` | Nonblocking failure with matching version/status and an explicit reason. | Record resolution without marking the delivery as successfully processed. Blocking open/retrying state gives `sys.failure.state_conflict`; callers must retry or explicitly skip it. No checkpoint movement. |

Each returned retry/replay handle follows [task ownership](#execution). An administrative request admitted as a direct REPL start may be session-owned; an operation that calls it and then ends cancels unfinished work. The lifetime never depends on whether a client happens to keep the handle in memory.

## Cancellation and crash recovery

**ORNA-ADMIN-003** A state-changing command MUST expose one of: no admission, admitted and recoverable, or terminal. On reconnect, an unknown outcome MUST be inspected through its retained invocation/journal identity before another non-idempotent command is issued.

The host must distinguish inability to report a response from inability to commit an operation. A disconnected response after a successful commit does not turn that commit into a failure. A retry with the same idempotency identity observes that recorded outcome. External effects remain subject to the limitations in [execution](#execution); the administrative interface does not make an arbitrary provider exactly-once.



## Reviewed commit boundary (1.1)

`sys.admin.review_commit()` validates the current staged candidate and returns a `sys.CommitReview`. It performs no staging or branch mutation. It fails on unresolved index entries, invalid candidate schema/rows/Blob closure, absent required objects or an unavailable CWD pin. The candidate is the actual complete index tree, not a list of display labels.

The review token is `SHA256(ASCII("orna.commit-review.v1") || 00 || OVB-1(payload))`, where payload is the ordered array `[database_uuid, runtime_uuid, head_oid_or_null, head_ref_or_null, index_tree_oid, index_digest, worktree_digest, cwd_snapshot_descriptor, pending_generation, repository_format_uint]`. OIDs and digests use the existing typed encodings; absent structural fields use CBOR null. `index_digest` is SHA-256 over the captured complete index-file bytes. `worktree_digest` is SHA-256 of `orna.commit-review-worktree.v1`, a zero byte, and OVB-1 of an array sorted by repository-path UTF-8 bytes, with entries `[safe_relative_path, git_mode_uint, SHA256(current_file_bytes)]` for all candidate-managed paths whose ordinary worktree content will be reconciled; absent paths use mode 0 and null digest. It is an explicit full byte observation of these ordinary files, not external media payload hydration. The journal retains that exact path set; extra managed paths discovered at final validation invalidate the review. Sparse excluded content paths are omitted from this worktree byte inventory and are already bound by `index_tree`; they MUST NOT be hydrated merely to produce a review. Symlinks are hashed as their raw link-target bytes without following them. Gitlinks bind the checked-out dependency HEAD OID as ASCII without a newline; the publisher does not reconcile or rewrite files inside the dependency. A split-index reader must validate every referenced shared index before accepting the complete index tree and flags.

`sys.admin.commit_reviewed(review, message, author)` rechecks administrative authority and recomputes/compares every review input under the ordinary index lock plus publication lock. It then creates and publishes exactly the reviewed candidate using the same ref CAS, durability and recovery rules as `sys.admin.commit`. Concurrent changes fail `sys.git.stale_review`; the command does not silently refresh the review and retry. Supplying a forged review cannot grant authority. Empty candidates follow `sys.git.nothing_to_commit`. Message/author are bound at invocation; their omission from the review token permits drafting a message after reviewing the contents. The implementation uses the existing versioned operation/request identity for lost-result recovery and does not call this operation again with a new identity after an uncertain result.

The older `sys.admin.commit` remains unchanged and is not retrospectively described as a reviewed-candidate operation. UI controls that promise to commit reviewed contents must use the guarded operation.

`compact` is physical maintenance of the single recorded profile, not a storage-mode selector. Legacy segment-named result fields are retained only as compatibility-shaped observations; their items identify bounded row pages under the current schema. The new API marks this deliberately rather than maintaining Parquet as another writer.
