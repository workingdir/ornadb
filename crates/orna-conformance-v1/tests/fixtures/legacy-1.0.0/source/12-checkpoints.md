# 12. Checkpoints and failed deliveries {#checkpoints}

A checkpoint records how far one durable consumer has committed. A failure record describes one preserved delivery that could not be processed. Neither is identified by a process ID, a display label or an incrementing retry number.

## Checkpoint identity and position

**ORNA-CP-001** Current checkpoints MUST be stored transactionally in Turso with the table writes produced by the corresponding item or batch.

**ORNA-CP-002** Publication MUST include checkpoint watermarks corresponding exactly to the included rows. It cannot publish a later watermark while omitting writes required by that watermark.

**ORNA-CP-003** `sys.Checkpoint` exposes current checkpoint state and committed historical watermarks through snapshot selection.

**ORNA-CP-004** A position-format change MUST carry a version. Incompatible formats require explicit migration, reset or adoption; bytewise or textual ordering cannot substitute for a provider-defined comparator.

A checkpoint row is keyed by `(consumer_identity, source_identity, partition)`. `sys.ConsumerIdentity` includes the database, stable consumer function identity and canonical bound arguments. Its key therefore distinguishes two runs of the same function with different source configurations. `partition = null` denotes the single unpartitioned source; it is not an empty-string partition.

`sys.CheckpointPosition` retains the provider and format identity with its opaque bytes. Generic tooling can compare two positions for equality only when those identities agree. It cannot infer that a numerically or lexicographically larger token represents later progress.

`sys.consumer_identity(function, arguments)` computes the durable identity. `sys.checkpoint(consumer_identity, source_identity, partition: ...)` retrieves the corresponding row or `null`. A bare function value, a `sys.FunctionRef`, a display name and a consumer identity are not interchangeable.

## Processing algorithm DELIVERY-1

1. Obtain the durable consumer lease and load its current checkpoint/version. A newly registered consumer obtains the provider's initial position and an initial version before reading its first delivery.
2. Fetch an item or bounded ordered batch outside the write transaction. Record its source identity, partition, delivery position and any provider-defined successor position. Preserve enough payload or protected refetch information to recover the delivery if processing fails.
3. Begin an activation against one CWD generation. Invoke the handler; all Orna table writes belong to this activation. Validate references and assertions.
4. Compare the current checkpoint with the expected version and starting position. Atomically commit handler writes and the new checkpoint. A concurrent reset or owner change fails the comparison and rolls the activation back.
5. On ordinary failure, roll back handler writes and checkpoint movement. In a separate short metadata transaction, create or update the single failure record for this delivery and pause further ordered delivery admission.
6. On cancellation, roll back the open activation. A cancellation alone does not manufacture an ordinary processing error. If a retry record already exists, release its execution lease and return it to the appropriate recoverable state after rollback is established.

**ORNA-CP-005** Activation failure MUST roll back both table writes and checkpoint advancement.

**ORNA-CP-006** An ordered consumer MUST NOT advance beyond a failed delivery unless that exact delivery is explicitly skipped with a provider-supported successor.

## One record per delivery

**ORNA-FAIL-001** Repeated processing failures for the same delivery MUST update one durable failure record. Retry number is state, not identity; failed retries MUST NOT append successor failure rows.

**ORNA-FAIL-002** A retry limit MUST NOT silently discard a delivery. The source remains paused, or an explicit bounded retry policy retries the same delivery.

**ORNA-FAIL-003** Explicit skip MUST atomically preserve the delivery's payload or protected refetch reference, update the existing failure record to `skipped` and advance the checkpoint past exactly that delivery. If no failure record exists, the same operation creates that one record; it does not create a second identity for an already recorded delivery.

**ORNA-FAIL-004** Replay of a skipped delivery runs the current compatible handler on preserved/refetched data without rewinding or advancing the live checkpoint.

**ORNA-FAIL-005** Failure identity MUST be the exact composite `(consumer_identity, source_identity, partition, position_format, position)`. A position digest is an index accelerator only; equality still verifies the complete typed position. `sys.FailureRef` encodes that natural identity and does not add a synthetic delivery ID.

The row has `version: sys.FailureVersion`, `attempt_count`, latest safe `error`, timestamps, payload/refetch metadata, checkpoint preconditions and current status. `attempt_count` counts admitted processing attempts, including an attempt interrupted before its external call completed. It is not a promise about exactly how many times an external provider observed an effect. Each durable state transition changes `version`.

Optional bounded trace records may describe individual attempts. They are not authoritative failure identities, cannot grow without a retention policy and cannot be required to locate the current blocked delivery.

## State machine

| Starting state | Operation | Result |
|---|---|---|
| No row | Handler fails | One `open` row, attempt count 1, unchanged checkpoint. |
| `open` | Accept retry under version/checkpoint CAS | Same row becomes `retrying`; count increases; one execution lease. |
| `retrying` | Handler succeeds | Writes, checkpoint movement and `recovered` transition commit together. |
| `retrying` | Handler fails or is cancelled | Writes roll back; same row returns to `open`; checkpoint unchanged. |
| `open` | Authorised skip | Same row becomes `skipped`; preserve delivery and advance checkpoint atomically. |
| `skipped` | Accept replay | Same row becomes `replaying`; live checkpoint is not involved. |
| `replaying` | Handler succeeds | Replay writes and `replayed` transition commit together; live checkpoint unchanged. |
| `replaying` | Handler fails or is cancelled | Replay writes roll back; same row returns to `skipped`. |
| `skipped` or `replayed` | Acknowledge | Same row becomes `resolved`; no table replay or checkpoint movement. |

`recovered` and `resolved` are terminal administration states. A later explicit source reset may cause the same delivery to be encountered again; its stable identity remains the same, and a newly failed processing cycle reopens the row under a new version rather than inventing a new key. Its prior successful audit metadata remains in bounded invocation/change history. A transition cannot be inferred solely from the display status; the version is always checked.

**ORNA-SYS-061** Successful item/batch writes and checkpoint movement MUST commit atomically.

**ORNA-SYS-062** Failure or cancellation MUST leave the committed checkpoint unchanged and roll back the interrupted handler's writes.

**ORNA-SYS-063** Checkpoint changes MUST compare both the expected checkpoint version and typed position.

**ORNA-SYS-064** Stale checkpoint preconditions MUST fail with `sys.checkpoint.conflict`. Stale failure-row versions MUST fail with `sys.failure.stale_version`. Neither failure permits handler execution or progress movement.

**ORNA-SYS-065** A replayable provider promises resumption only under its declared delivery contract; this MUST NOT be represented as exactly-once effects in arbitrary external systems.

**ORNA-SYS-066** Payload retention MUST obey the secret/redaction rules. A protected reference and digest may replace plaintext retention, but the exact source position remains part of delivery identity.

**ORNA-SYS-121** `retry_failure` MUST compare the failure's expected version/status and the blocking checkpoint precondition, acquire the sole delivery lease, and durably mark the same row `retrying` before user code executes.

**ORNA-SYS-122** A successful retry MUST atomically commit its writes, checkpoint movement and `recovered` state. A failed retry MUST keep the same failure identity, leave the checkpoint unchanged, update the diagnostic/version and return the row to `open` after rollback.

**ORNA-SYS-123** `replay_failure` MUST execute preserved nonblocking data in a separate activation. It MUST NOT move the live checkpoint; failure returns the row to `skipped`, and success commits its writes with the `replayed` transition.

**ORNA-SYS-124** `resolve_failure` MUST NOT acknowledge away a blocking `open` or `retrying` record. Only successful processing or an explicit atomic skip can clear that progress barrier.

## Administration and stale tools

Every retry, skip, replay and resolve call supplies `expected_version`. Status alone is insufficient: a row may go from `open` to `retrying` and back to `open` while a tool still holds its earlier observation.

A safe interaction reads a typed failure row, displays the intended action and submits its reference and version. If the row has changed, the tool refreshes it and asks for new operator intent where the action is destructive; it does not automatically retry a stale skip with newer preconditions.

Checkpoint reset requires a paused consumer, no in-flight callback and a matching expected version/position. A current blocking failure must first be resolved by retry or skip; reset cannot turn a blocking error into an unrecorded discard. A format-incompatible reset is rejected. The audit row records the old/new typed positions and reason, with secrets redacted.

If the runtime crashes while an attempt is marked `retrying` or `replaying`, recovery first establishes that its invocation transaction did not commit. It then fences the expired lease and returns the same record to `open` or `skipped`. If commit did occur, recovery completes the matching terminal metadata instead. It never starts a second callback while the first can still commit.

## Recovery between clones

**ORNA-CP-007** Restart on the same worktree MUST resume from its durable Turso checkpoint.

**ORNA-CP-008** A different clone MUST resume from the checkpoint present in its selected fetched snapshot, not a guessed local copy of another clone's tail.

**ORNA-CP-009** Recovery of unpublished data after machine loss requires replayable source data or a separately preserved local tail. Git replication alone does not contain unpublished state.

