# 21. Branching, staging and checkout {#branching}

## Branches and pending changes

A branch is a Git reference to a commit, not a container for uncommitted work. `HEAD` selects the current branch or a detached commit. The worktree, its staging area and its local durable CWD overlay belong to the worktree instance.

```bash
git branch experiment       # create at HEAD; remain on the current branch
git switch -c experiment    # create at HEAD and select it
```

The corresponding `orna` commands preserve these meanings. Neither command creates a commit. Creating and selecting a branch at the current `HEAD` carries staged changes, unstaged changes and pending logical database changes unchanged. The new branch's committed history remains the same as the old `HEAD` until a commit is made.

**ORNA-BRANCH-001** Creating a branch without an explicit start point MUST resolve the current Git `HEAD`, create the new branch by compare-and-set against nonexistence, and leave CWD and the index unchanged. The operation MUST NOT commit pending data.

**ORNA-BRANCH-002** Selecting a new branch at the same commit MUST preserve the staged/unstaged distinction, pending table changes, local allocator high-water marks and source checkpoints. A branch switch cannot reset an allocator or discard unpublished rows merely because they are not in Git yet.

**ORNA-BRANCH-003** Switching to another commit MUST carry nonconflicting local changes using Git-compatible worktree/index rules and equivalent logical row/schema checks. If a change would be overwritten, or the resulting candidate database would be invalid, the default operation MUST fail with CWD, index and `HEAD` unchanged.

**ORNA-BRANCH-004** A historical snapshot containing no Git commit is not a branch start point. An unborn `HEAD` fails `create_branch` without an explicit committed start point. Selecting a new unborn symbolic branch with `orna switch -c` is separately defined in [administration](#administration). Orphan-branch creation remains a separate explicit Git operation and does not manufacture an empty commit.

An explicit committed start point may be a commit, peeled tag or another branch. Snapshot references must belong to this repository or resolve to a commit reachable through an explicitly attached repository import; a database identifier alone does not authorise cross-repository reference creation.

### Staging logical changes

`orna add` stages the selected logical CWD generation for the named paths or tables. Later edits remain unstaged. For loose rows, the staged tree is the normal Git index. For pending compact rows, Orna maintains a staged logical delta bound to the index generation, and builds its staged immutable objects without consuming newer unstaged deltas. A stage record stores its base commit, per-key before/after state and schema revision.

`orna commit` and `sys.admin.commit` commit the staged state, not every value visible in CWD. They validate the staged candidate schema and assertions, build a commit from that candidate, and preserve unstaged changes against the resulting commit. An empty staged delta is a no-op error unless an explicit Git-compatible empty-commit option is used. Automatic publication has a distinct rule: it publishes only the frozen runtime batch and never unrelated user staging.

A plain Git commit can operate on materialised staged paths. It cannot implicitly publish rows that have never been materialised or staged into its index. `orna status` reports that remaining local tail. A later Orna command reconciles the actual `HEAD` with its index-generation record and refuses stale staging instead of silently selecting an old overlay.

### Checkout preview and consent

`sys.admin.plan_checkout(target)` is read-only. It resolves the target and returns `sys.CheckoutPlan`: the exact target commit/branch, expected `HEAD`, CWD generation, index and worktree digests, pending generation, affected consumers, conflicts and any changes a destructive checkout would discard.

The plan token is SHA-256 over the canonical typed plan, excluding the token itself. It is a state precondition, not an authentication credential. `sys.admin.checkout(..., expected_plan: token)` rechecks it while holding the worktree mutation lock. Any relevant change produces `sys.git.stale_plan`.

A nonforced checkout needs no preview token when it can recompute and apply a safe carry-forward under the lock. A forced checkout requires both `force: true` and a matching plan token. The host must have obtained explicit consent for the listed discard set; a noninteractive caller expresses that consent by supplying both arguments. Force cannot bypass schema validity, assertions, object availability, active-transaction fencing or repository-integrity checks.

The target form preserves attachment semantics: a `BranchRef`, or a string resolving unambiguously to a local branch, selects that branch; an exact commit, snapshot, object ID or tag selects detached `HEAD`. No implicit branch guessing occurs for an ambiguous string.

### Checkout algorithm CHECKOUT-1

1. Acquire the worktree mutation lock, observe the current symbolic or detached `HEAD`, and prevent new activation admission during the switch.
2. Wait for ordinary active transactions to reach a boundary. A forced operation may request their cancellation and join cleanup, but may not terminate unrelated worktrees. Reject reentrant checkout from an activation that owns uncommitted table writes.
3. Resolve the committed target, obtain required objects and validate its source graph, schema, stored values, assertions and checkpoint formats in isolation.
4. Compute the staged, unstaged and pending logical carry-forward. Conflicting local edits cause `sys.git.dirty_conflict` unless the exact destructive plan has been authorised. Untracked or ignored paths are never overwritten as an incidental consequence of resolving a database change.
5. Pause affected consumers at completed item/batch boundaries. Preserve their worktree-local progress when switching to the same state or carrying compatible changes. If their checkpoint/code/source configuration is incompatible, leave them paused and report the reason; do not silently restart at a guessed position.
6. Recheck the plan, index, worktree and `HEAD` preconditions. Write a durable transition journal containing before/after state and exact discard consent, then apply the Git index/worktree and logical CWD transition as one recoverable generation change.
7. Publish the selected `HEAD` and CWD generation only when their relationship is recoverable. Existing pinned readers continue reading their old generation; new readers observe the new generation. Complete the journal and release admission.
8. Resume only consumers proven compatible with the resulting state. A consumer requiring an explicit reset remains paused. Return the selected snapshot and make the operation inspectable through invocation/change metadata.

On a crash, recovery compares journalled object identities and generations; it completes the recorded transition or preserves the old state. It never runs a blind hard reset over subsequently changed files. Unexpected external edits are preserved and reported as conflicts.

