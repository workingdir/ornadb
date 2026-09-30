# 19. Repository, CWD, index and HEAD {#repository}

## Required repository form

A minimal database has this shape:

```text
example/
├── .git/
├── main.orna
└── ... reachable modules and data ...
```

**ORNA-REPO-001** `main.orna` at the repository root MUST be the root module.

**ORNA-REPO-002** The module graph, not a recursive scan of every `.orna` file, MUST determine which module units form the database program.

**ORNA-REPO-003** Files not reachable from the root module MUST NOT contribute declarations to the database merely because they exist.

**ORNA-REPO-004** Row units belonging to a reachable table are database data even though row units are not imported individually.

## CWD, index and HEAD

Orna preserves Git's three human-facing states while adding durable runtime changes:

```text
HEAD      committed snapshot
INDEX     changes staged through Git/Orna
WORKTREE  ordinary checked-out file changes
RUNTIME   durable Turso-backed pending rows and system state
CWD       HEAD + INDEX/WORKTREE differences + RUNTIME differences
```

**ORNA-STATE-001** `HEAD` MUST mean the actual selected Git commit and MUST NOT silently include uncommitted hot data.

**ORNA-STATE-002** Plain relation access MUST default to CWD.

```orna
directory.Contact
```

is equivalent to:

```orna
directory.Contact.as_of(CWD)
```

**ORNA-STATE-003** `relation.as_of(HEAD)` MUST read the selected committed snapshot without the local runtime tail.

**ORNA-STATE-004** `relation.as_of(snapshot)` MUST use the exact supplied SnapshotRef. Git revision expressions are resolved explicitly through `sys.snapshot(selector)`; a bare branch expression is not additional source grammar.

**ORNA-STATE-004A** Relation-level `as_of` MUST pin data and decoding schema, not silently select historical function code. Whole-program historical evaluation uses the database snapshot object defined in [relations](#relations).

**ORNA-STATE-005** Wall-clock `now()` MUST remain distinct from database-state selectors such as `CWD` and `HEAD`.

## Committed Orna metadata

A small tracked `.orna/` directory is reserved for canonical metadata that must exist in every snapshot but is not user schema source. It is distinct from local runtime files under `.git/orna/`.

```text
.orna/
├── format.orna
├── database.orna
├── checkpoints/
├── allocators/
├── runs/
└── failures/
```

**ORNA-META-001** Tracked `.orna/` MUST contain versioned metadata required to interpret or inspect a snapshot. It MUST NOT contain local caches, credentials or the authoritative unpublished runtime tail.

**ORNA-META-002** `.orna/format.orna` MUST identify the repository format and storage profile versions.

**ORNA-META-003** `.orna/database.orna` MUST contain the stable database identity created by `orna init`.

**ORNA-META-004** Checkpoint and allocator files in `.orna/` are snapshot watermarks; their current non-rewindable/local counterparts remain in Turso or hidden refs.

**ORNA-META-005** Implementations MUST NOT store rebuildable query/compiler caches in the tracked `.orna/` directory.

## Local runtime files

The embedded runtime requires local state that is not ordinary repository content.

**ORNA-LOCAL-001** Local runtime state MUST live under a path resolved through Git's per-worktree administrative directory, conceptually `git rev-parse --git-path orna/`.

A typical layout is:

```text
.git/orna/
├── state.db       # Turso: authoritative uncommitted runtime CWD
├── cache.db       # rebuildable compiler/query/storage indexes
├── runtime.sock   # optional local coordination socket
└── locks/
```

**ORNA-LOCAL-002** `state.db` MUST be treated as authoritative for runtime changes that have not yet been published to Git.

**ORNA-LOCAL-003** `cache.db` MUST be rebuildable from Git plus `state.db`.

**ORNA-LOCAL-004** Local runtime files MUST NOT be added to ordinary Git commits.

## Any clone may operate independently

**ORNA-EMBED-001** An Orna clone MUST remain usable without `orna serve` or any external Orna daemon.

**ORNA-EMBED-002** `orna repl`, `orna run`, `orna status` and other local commands MUST be capable of opening the embedded Turso state in-process when no other local owner exists.

**ORNA-EMBED-003** When overlapping local processes require one writable Turso handle, they MAY coordinate through a local socket and temporary owner process; this MUST remain an implementation detail and MUST NOT require a network server.

**ORNA-EMBED-004** Version 1.0 MUST NOT rely for correctness on experimental multi-process Turso file access.

## Independent clones

Each clone has its own CWD.

**ORNA-CLONE-001** Orna MUST treat each clone's CWD as independent.

**ORNA-CLONE-002** Commits, fetch, push and merge are the synchronization mechanism between clones.

**ORNA-CLONE-003** Two clones MAY make independent changes. Non-fast-forward pushes and semantic merges MUST be handled through Git-compatible workflows rather than hidden distributed locking.

**ORNA-CLONE-004** Independent clones MUST NOT run the same durable external consumer concurrently unless its connector profile explicitly supports safe partitioning, coordination or idempotent duplicate processing. Base conformance provides no cross-clone exactly-once guarantee.


## Repository compatibility

**ORNA-CORE-001** An Orna repository MUST be a valid Git repository readable by ordinary Git tooling.

**ORNA-CORE-002** Orna MUST NOT substitute a proprietary commit graph for Git.

**ORNA-CORE-003** A Git commit used as an Orna database snapshot MUST identify the code, schema, human-readable rows, large-table manifests, referenced large-table objects and committed system watermarks required to reproduce that snapshot.
