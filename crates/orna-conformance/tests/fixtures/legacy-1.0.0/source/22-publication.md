# 22. Publication and crash recovery {#publication}

The logical CWD combines committed rows with durable pending mutations. Publishing changes their representation and replication boundary: it makes an already committed local activation available through a Git commit. It is not a second execution of that activation.

## Pending state and policy

**ORNA-HOT-001** High-rate writes MUST become durable and queryable in embedded Turso before publication.

**ORNA-HOT-002** Status output MUST summarise pending high-rate changes rather than emit one path per pending row.

**ORNA-PUB-001** The default compact writer target is 16 MiB of compressed data, with an 8–32 MiB normal tuning range and the compact profile's 64 MiB file bound. An implementation MAY choose a target within that range but MUST expose the effective policy. A changed tuning value does not change logical rows or checkpoint meaning.

**ORNA-PUB-002** A maximum pending age MAY cause publication of a smaller complete batch; the profile default is 60 seconds.

**ORNA-PUB-003** Publication policy MUST NOT introduce a different table declaration kind.

**ORNA-PUB-004** Effective policy and pending/published state MUST be visible through `sys.Storage` and runtime maintenance metadata.

## Index and worktree invariants

Let **H** be the captured old commit tree, **N** the proposed publication tree, **I** the ordinary Git index, **W** the ordinary working tree, and **P** the frozen runtime mutation batch. The publisher builds **N** from **H + P**, not from **I** or **W**.

The publisher must also compute a reconciled index **I′** and worktree **W′**. Their purpose is to preserve the user's staged difference from the new `HEAD` and the user's unstaged difference from the new index. Merely changing `HEAD` while leaving **I = H** would stage the inverse of **P**. A later ordinary commit could then reverse the publication.

For every managed path changed by **P**, automatic publication requires that its existing index/worktree entries agree with the captured managed base or a journalled Orna projection of the same batch. A conflicting human edit pauses publication. For unrelated paths, staged and unstaged differences are preserved independently, including partially staged files, deletions, file modes and untracked paths. The publisher does not use a hard reset or a blanket `git add`.

**ORNA-PUB-010** A publication commit MUST use a private tree/index built only from the captured base and frozen runtime batch. It MUST NOT commit unrelated human edits.

**ORNA-PUB-011** Before publication is reported complete, the ordinary index MUST be reconciled so it contains no accidental staged reversal of published managed paths, while preserving every unrelated staged change and staged/unstaged boundary.

**ORNA-PUB-012** Unresolved merges, rebases, overlapping managed-path edits or unreconcilable index state MUST pause automatic publication. Durable ingestion may continue within configured storage limits; exhaustion fails explicitly rather than discarding the tail.

## Publication algorithm PUB-1

The following ordering specifies observable durability and recovery requirements. An implementation may use equivalent platform primitives but cannot omit ordinary-index reconciliation.

1. **Freeze.** In a short Turso transaction, select a contiguous committed mutation range, allocate its batch identity, and record the included source/checkpoint watermarks and base generation. Mark it `preparing`; do not remove the pending mutations. New activations may append after this range.
2. **Capture.** Under the worktree publication lock, capture the symbolic/detached `HEAD` identity and old object ID, normal index digest and relevant worktree states. Refuse conflicting merge/rebase state and managed-path edits. Resolve `.git` paths through Git's per-worktree administrative directory.
3. **Encode.** Produce immutable segments, schemas and manifests for the frozen batch. Fold repeated mutations in transaction order to one authoritative final mutation per key. Write and flush complete Git objects. Objects not subsequently referenced may remain unreachable.
4. **Build candidate.** With a private index, build **N = H + P** and a commit whose parent is the captured `HEAD`. Human staged or unstaged contents are not inputs. A table publication generation comes from that table's captured manifest.
5. **Plan reconciliation.** Compute **I′** using a Git-compatible two-tree carry-forward from **H** to **N**, preserving unrelated staged changes. Compute **W′** preserving unrelated unstaged changes. Reject managed-path overlaps. Validate staged and CWD database candidates separately; carrying human work cannot make either candidate silently invalid.
6. **Journal.** Durably record the batch, expected/target refs, old/new index bytes or immutable equivalents, affected old/new worktree entries, object hashes, generation and cleanup watermark. Journal data is local metadata, not ordinary committed source. It must be sufficient to distinguish every crash boundary below.
7. **Lock and revalidate.** Acquire the ordinary Git `index.lock` before exposing a changed ref. Hold it until the reconciled index is installed. Obtain the Git ref transaction/expected-old-value protection and recheck the captured symbolic `HEAD`, index and relevant files. Abort if they changed. Cooperating Orna worktree mutators remain excluded by the publication lock.
8. **Prepare files.** Write temporary replacement files and flush them. For each existing affected file, preserve the displaced bytes in the recovery journal/quarantine before installing a replacement. If an external editor changed the path after capture, retain those bytes and stop with a conflict rather than overwrite them. Filesystem writers outside the locking protocol cannot participate in the multi-file transaction. Their conflicting edits must be preserved for reconciliation.
9. **Advance and reconcile.** Advance the target ref using compare-and-set against the old object ID. While the ordinary index remains locked, atomically install **I′** and finish the planned worktree replacements, recording progress. If an external Git process moves a ref after the protected update, recovery treats it as a new state to reconcile; it never forces the ref back.
10. **Publish local boundary.** In Turso, record the publication commit and completion state, then consume only the batch's frozen pending range. Newer tail mutations remain. Logical readers switch using the batch watermark so a mutation is visible exactly once: either through the old commit plus tail, or through the new commit with that batch masked out of the tail.
11. **Complete.** Flush the final journal state, release the normal index lock and worktree lock, and mark the batch complete. Notify storage observers. Do not emit an insert/delete row delta merely because a row moved into a compact file.

**ORNA-PUB-005** Complete objects and their required durability barriers MUST precede a visible ref that names them.

**ORNA-PUB-006** Failure before ref advancement MUST preserve the pending batch. Unreferenced objects are permitted; a false successful publication result is not.

**ORNA-PUB-007** Failure after ref advancement MUST recover index/worktree reconciliation as well as Turso cleanup. Recognising the batch in the committed manifest is necessary but not sufficient.

**ORNA-PUB-008** A reader MUST observe the old snapshot plus the unpublished tail or the new snapshot with the published batch excluded from the tail. It MUST NOT observe duplicate or missing logical rows.

**ORNA-PUB-009** Ref updates MUST use expected-old-object compare-and-set and revalidate the selected branch/`HEAD` relationship. Concurrent commits MUST NOT be overwritten.

**ORNA-PUB-016** Recovery MUST preserve unrelated staged and unstaged changes. An index lock left by a crashed publisher may be removed only after owner-liveness and journal checks establish that it is that publisher's abandoned lock.

**ORNA-PUB-017** A normal Git commit made after completed publication MUST not reverse published managed data unless the user explicitly staged such a reversal.

### Recovery states

| Observed state | Required action |
|---|---|
| Ref still at H; no candidate visible | Restore only journal-owned partial projections when their hashes match; keep P pending; release abandoned locks safely. |
| Ref at N; ordinary index still I | Complete I′ and W′ reconciliation from the durable journal before admitting further Orna commits; preserve unexpected edits as conflicts. |
| Ref at N; index I′; cleanup absent | Verify the batch manifest, apply the visibility watermark and finish idempotent tail cleanup. |
| Ref at N; cleanup complete | Verify journal completion; remove only temporary/quarantined data no longer needed and explicitly resolved. |
| Ref no longer H or N | Preserve journal and user files, inspect ancestry/batch presence and reconcile under the new ref. If safety cannot be proved, stop with a recovery conflict. Never overwrite the newer ref. |
| Index or affected path differs from both recorded states | Preserve that state; report a typed index/worktree conflict. Do not interpret it as permission to reset. |

A ref update and an index-file rename are not one filesystem transaction. The lock, journal and recovery procedure establish the safe boundary. A command that bypasses Git's index lock and writes arbitrary files is outside cooperative execution, but its unexpected contents must still be preserved when detected.

## Shutdown and transfer

**ORNA-PUB-013** Graceful writer shutdown SHOULD publish eligible batches after its children have terminated and open activations have reached a boundary. A pending publication conflict does not justify discarding the local tail.

**ORNA-PUB-014** Status MUST report remaining unpublished CWD changes.

**ORNA-PUB-015** Transferring resumable execution to another clone requires publishing and pushing the recoverable state, then fetching the resulting snapshot in the receiving clone. Loss of an unpublished local tail can be repaired only by replayable source data or an independently preserved copy of that tail.

Example publication message:

```text
orna: publish runtime data

sensors.Reading     48,120 rows
warehouse.Event       240 rows
```

