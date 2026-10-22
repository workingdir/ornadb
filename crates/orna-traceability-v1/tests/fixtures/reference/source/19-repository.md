# 19. Repository, CWD, index and HEAD {#repository}


## One executable snapshot

**Conceptual integrity:** Git is the published database, not an optional export
of it. One commit binds program, schema, table data, binary content and exact
external dependency gitlinks. A clone can acquire omitted objects lazily;
logical reconstruction does not mean downloading or recompiling everything.

```text
library/
  main.orna
  music.orna
  .gitmodules                 # only when dependencies require it
  stdlib/std/                 # explicit pinned dependency, when selected
  .orna/
    database.orna
    store/                    # native Git tree, normally sparsely excluded
  .git/                       # directory OR gitfile; not a fixed runtime path
```

**ORNA-REPO-001** `main.orna` MUST remain the fixed root module.
**ORNA-REPO-002** The reachable module graph MUST determine the database program; recursive filename scanning is not module admission.
**ORNA-REPO-003** Unreachable `.orna` files MUST NOT acquire declarations or execute because they exist.
**ORNA-REPO-004** Stored rows MUST belong to stable reachable table identities in the format-3 store; rows are not individually imported modules or executable scripts.
**ORNA-CORE-001** Published repositories MUST use ordinary Git objects, refs and commits readable by ordinary Git tooling.
**ORNA-CORE-002** The implementation MUST NOT substitute a proprietary history graph, LFS, or a separate mandatory media service.
**ORNA-CORE-003** A published commit MUST bind complete source/schema/data/content closure and exact dependency gitlinks; local availability and globally complete snapshot identity MUST be distinguished.

## HEAD, index and CWD

`HEAD` is the selected Git commit. The ordinary index is Git's staged tree and
flags. The worktree contains checked-out source/resource edits. `state.db`
contains accepted but unpublished logical changes. CWD is the admitted
composition of these observations, never an arbitrary last-writer union.
Overlapping managed-store or schema edits are explicit conflicts.

**ORNA-STATE-001** `HEAD` MUST exclude uncommitted runtime data.
**ORNA-STATE-002** Plain relation access MUST select CWD.
**ORNA-STATE-003** `relation.as_of(HEAD)` MUST exclude the unpublished tail.
**ORNA-STATE-004** Explicit snapshots MUST resolve exactly, not silently rebind to a later branch head.
**ORNA-STATE-004A** Relation-level `as_of` MUST pin data and its decoding schema; whole-program historical execution is separately selected.
**ORNA-STATE-005** Database selectors MUST NOT replace wall-clock `now()`.

## Exactly two required tracked metadata entries

`database.orna` is a bounded DATA record, not configuration code:

```orna
{
    repository_format: 3,
    database_id: "9f237b7e-6844-4498-bcd5-d24641c07449",
}
```

The physical format coordinate is not the language or application version.
The UUID is generated once, preserved across branches/clones, and is not a
credential. The current commit cannot embed its own final hash. `.orna/store`
is already named by that commit's tree: no duplicate root hash is needed.

**ORNA-META-001** Format-3 tracked `.orna/` MUST contain only `database.orna` and `store`; required published system records MUST use the shared store.
**ORNA-META-002** `database.orna` MUST carry `repository_format: 3` and the stable database UUID; a separate `format.orna` is a legacy-format input, not another new-format authority.
**ORNA-META-003** Initialization MUST generate one UUIDv4, preserve existing source, and neither stage nor create a Git commit implicitly. Changing database identity requires an explicit fork-as-new-database migration.
**ORNA-META-004** Published checkpoint/allocator/runtime summaries MUST remain historical observations; checking them out MUST NOT rewind live fences, leases or non-rewindable allocators.
**ORNA-META-005** Caches, credentials, mounts, sessions, rejected edit drafts and unpublished changes MUST NOT be written into tracked `.orna/`.

## Private local ownership

Resolve administrative paths through Git (`git rev-parse --git-path orna`),
including linked worktrees. Do not append `/.git` to a guessed directory.

```text
<per-worktree-git-admin>/orna/
  state.db       # durable unpublished changes and journals
  cache.db       # optional, rebuildable
  scratch/       # bounded capture/edit/recovery candidates
  runtime.sock   # when an owner runs
  owner.lock     # when required by platform
```

WAL/SHM companions and required locks are real durability machinery, not
clutter to delete. Mount/edit records live in the same journal, not sidecar
JSON beside each row. Cache loss is recoverable; loss of unpublished accepted
state is not automatically recoverable from Git.

**ORNA-LOCAL-001** Private state MUST use the per-worktree administrative path.
**ORNA-LOCAL-002** Unpublished accepted state MUST be authoritative and crash-durable under the declared failure model.
**ORNA-LOCAL-003** Derived caches MUST be disposable and keyed by source/schema/store/interface generations.
**ORNA-LOCAL-004** Private state MUST never be added to ordinary commits or included by a naive working-directory copy advertised as a complete export.
**ORNA-EMBED-001** Local database use MUST work without devtools, a VFS mount, or `orna serve`.
**ORNA-EMBED-002** The local CLI MUST be able to own embedded state in-process when no local writer exists.
**ORNA-EMBED-003** Concurrent local commands MAY use private IPC to the single owner, not an obligatory public daemon.
**ORNA-EMBED-004** Correctness MUST NOT depend on experimental unsynchronized multi-process SQLite/Turso access.
**ORNA-CLONE-001** Clones and worktrees MUST have independent workspace/runtime identities; database UUID alone MUST NOT identify a writer.
**ORNA-CLONE-002** Published synchronization MUST use Git commits/fetch/push/merge.
**ORNA-CLONE-003** Non-fast-forward or concurrent writes MUST be explicit conflicts, not hidden cross-clone locks.
**ORNA-CLONE-004** Independent clones MUST NOT claim exactly-once external ingestion without a connector-specific coordination or idempotency contract.
