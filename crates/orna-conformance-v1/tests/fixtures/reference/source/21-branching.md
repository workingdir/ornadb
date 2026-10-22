# 21. Branching, staging and checkout {#branching}


Branches remain ordinary Git refs. Worktrees own pending state; a branch is
not a bag of unpublished changes. Mounts are projections of a worktree or a
pinned commit, not additional branch semantics.

**ORNA-BRANCH-001** Creating a branch at HEAD MUST preserve CWD/index and create no implicit commit.
**ORNA-BRANCH-002** Selecting a branch at the same commit MUST preserve pending data, staged/unstaged source differences and non-rewindable continuity state.
**ORNA-BRANCH-003** A different-commit checkout MUST run CHECKOUT-3 with exact target validation, conflict preservation and recoverable state transitions.

## Logical staging over a managed root

Source/resources use normal Git paths and index semantics. Rows no longer have
physical Git paths. `orna add --row TABLE KEY_TEXT` is the specified adapter
for staging a row's current stored value (or deletion) using a canonical
schema-typed key expression. `orna add --table TABLE` stages a whole selected
table only after showing/confirming its scope under the host's interaction
policy. These are metadata adapters, not new table representations.

The staging operation modifies a candidate `.orna/store` in the Git index and
records exact staged row generations. Later CWD mutations remain unstaged.
`orna restore --staged --row TABLE KEY_TEXT` restores that staged row to HEAD.
Do not claim ordinary `git add` can interpret virtual row paths.

**ORNA-STAGE-001** Staging MUST bind the exact selected source/schema/row/content candidate and preserve unrelated staged or partially staged paths.
**ORNA-STAGE-002** The index store MUST remain semantically complete and decode under its staged schema; missing dependencies or incompatible staged schemas MUST be rejected, not silently repaired using CWD.
**ORNA-STAGE-003** Repeated staging MUST be deterministic and metadata-only staging MUST NOT hydrate media payloads.
**ORNA-STAGE-004** Internal `.orna/store` paths MUST be managed atomically by Orna. Direct edits by other Git tools are detected as external candidate changes, not accepted without graph/schema validation.

## CHECKOUT-3

1. Resolve the selector once to an exact commit and, if applicable, an exact
   target ref. Acquire workspace mutation authority; fence new activations.
2. Wait for existing transactions to reach a boundary; cancellation joins owned
   children. Do not kill unrelated worktrees or quietly publish their work.
3. Admit target format, source graph, schema/catalogue and logical root.
   Verify available object structure; retain exact promises for lazy payloads.
   Validate affected row/reference/assertion obligations before visibility.
4. Compute staged/unstaged/pending carry-forward. Preserve safe independent
   edits; reject overlapping row/source/schema changes unless explicit reviewed
   resolution exists. A force token binds exact discarded-state identities.
5. Journal old/new HEAD/ref/index/worktree/store generations and continuity
   protection. Use Git-compatible index/ref locks and expected-old ref updates.
6. Install the coherent target and local base, leaving newer unrelated edits
   intact. Cache invalidation and VFS directory lookups switch by generation;
   existing read handles keep their pins. Unadmitted drafts are retained.
7. Complete recovery records before admitting new mutations. Do not rewind
   allocator authority, revive old sessions or rerun historical consumers.

**ORNA-CHECKOUT-001** A failed/stale checkout plan MUST leave current accepted state unchanged and preserve all drafts/unrelated changes.
**ORNA-CHECKOUT-002** Checkout recovery MUST inspect actual refs and journal progress; it MUST NOT force a ref back after another writer advances it.
**ORNA-CHECKOUT-003** A commit mount MUST be immutable; a branch-name snapshot mount MUST resolve once rather than move during reads.
**ORNA-CHECKOUT-004** A workspace mount MUST share the selected worktree's CWD, not create an untracked independent copy. Independent work requires another worktree/clone.
