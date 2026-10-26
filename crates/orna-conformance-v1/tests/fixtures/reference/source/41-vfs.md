# 35. Editable row-directory filesystem {#vfs}


## One interface to the stored database

VFS-1 is a case-sensitive Linux FUSE profile. Writable workspace mounts are
required for this profile; historical mounts are immutable. Other operating
systems require separately tested host adapters and filename rules. Portable
repository content does not imply identical host filesystem capabilities.

```text
Music/london/
  data.orna
  content.mp3
  cover.jpg
```

```orna
{
    title: "Live in London",
    artist: "Kieran",
    content: { path: "./content.mp3", media_type: "audio/mpeg" },
    cover: { path: "./cover.jpg", media_type: "image/jpeg" },
}
```

**ORNA-VFS-001** A virtual row directory MUST address database/table/typed key; `data.orna` MUST encode stored non-key fields only, under that snapshot's schema.
**ORNA-VFS-002** Virtual row text and sibling content files MUST be projections of the shared row store, not materialized competing authoritative copies.
**ORNA-VFS-003** Unchanged schema-directed Blob references MUST resolve internally in the captured row/draft context without reopening the mount or reading payloads.
**ORNA-VFS-004** Relative paths MUST stay within that field's registered projection. Absolute paths, symlink/traversal escapes and implicit cross-row references MUST fail.
**ORNA-VFS-005** Commit selectors MUST pin one snapshot; a branch-name snapshot selector resolves once. Workspace mounts MUST share one selected worktree's durable CWD.
**ORNA-VFS-006** Every accepted edit MUST use normal transaction/assertion/reference/permission validation and never automatically create a Git commit.

## Save state machine: EDIT-1

States: `clean -> dirty -> validating -> accepted`, or `rejected/conflicted`.
The journal records target identity, baseline row/schema/code generation,
owner, draft sequence, extent map, quota and unique apply identity. A writer
sees its own draft; other opens see accepted state. Read handles and directory
enumeration cursors pin coherent generations.

`write`, `truncate`, sparse holes and out-of-order byte writes change only a
private disk-backed/COW candidate. A zero-length Blob can be legal; validity
comes from schema constraints, not guessing whether the user intended it.

1. `fsync`/`fdatasync` of a managed file captures one draft sequence under its
   handle lock, parses/captures the entire candidate, then validates in the
   current authority context against the expected baseline.
2. New chunks are durably native-pinned before admission. Row and apply outcome
   commit atomically. Successful sync acknowledges durable accepted state.
3. Repeated callbacks for the same sequence return the recorded outcome. Later
   writes begin a new candidate based on accepted state; no duplicated mutation.
4. Failure leaves accepted data unchanged and retains an explicit recoverable
   draft/diagnostic. Ordinary `release` performs cleanup only, not acceptance.
5. A supported close `flush` attempts the same operation for dirty state, but
   may repeat or not occur and applications may ignore its error. Unsynced
   abandoned state is retained according to the stated bounded draft policy;
   it MUST NOT be advertised as durably accepted.

**ORNA-VFS-007** FUSE `release` MUST NOT be the only rejection/durability point; callback behavior and close-error limitations MUST be disclosed.
**ORNA-VFS-008** Dirty sequence capture and acceptance MUST serialize with writes; `dup`/fork/repeated flush MUST not create a second acceptance of the same candidate.
**ORNA-VFS-009** Invalid/stale candidates MUST preserve original accepted values, protect retained draft bytes and return a stable error/diagnostic association.
**ORNA-VFS-010** A metadata-only save MUST not fetch or rehash sibling content. A binary save MAY require streaming the entire resulting bytes to establish its raw SHA-256; memory remains bounded.

## Temporary-save replacement

An unknown file inside a row directory is private editor scratch, not a new
column. Its creation captures that row's current baseline. `fsync` of scratch
persists scratch only. Rename onto a managed target admits one complete
replacement and its namespace update through the same journal and transaction.
A failed rename keeps both old target and scratch recoverable. Unsupported
cross-directory/`RENAME_EXCHANGE`/whiteout behavior fails explicitly.

This does not prove when the editor originally read the row. If it closed an
old input handle and creates a new temp output much later, the VFS cannot infer
that prior read version. `orna edit PATH` captures a read-to-save baseline and
runs `$EDITOR` against an explicit draft, providing stronger CAS protection.
No compulsory revision comments or hidden row fields are added to `data.orna`.

**ORNA-VFS-011** Normal save CAS MUST compare the captured write/temp-create baseline. Strong initial-read protection MUST be claimed only for a retained edit token/guarded operation.
**ORNA-VFS-012** Renaming a suffix MUST not transcode bytes, rename columns or rekey a row. Compatible aliases update only the explicit suffix hint through CAS.
**ORNA-VFS-013** Removing a required Blob MUST fail; optional Blob unlink MAY set that field to null after validation. Deleting `data.orna` MUST fail; row deletion is an explicit database operation.
**ORNA-VFS-014** Hard links, device nodes, symlinks and writable shared mmap are unsupported in VFS-1 and MUST fail explicitly. Private mmap/read copies do not modify the database.
**ORNA-VFS-015** Multiple file saves MUST NOT be guessed into one transaction. `orna edit --row PATH` provides explicit grouped row/content admission; separate updates may reject cross-field constraints.

## Paths, naming and metadata

`profiles/vfs-1.md` specifies canonical escaping, composite keys, long-name
aliases, nested collections, field collisions and long-text projection.
`profiles/media-1.json` fixes MIME/suffix normalization/preferences. Path names
are not authority. Table keys remain valid regardless of host name limits.

**ORNA-VFS-016** Reserved/colliding names MUST be escaped reversibly, not rejected as otherwise valid database keys or merged by case-folding.
**ORNA-VFS-017** Directory iteration MUST be lazy/paged and pinned; lookup of a known key MUST use the index. An exhaustive recursive scan can still fetch all requested data.
**ORNA-VFS-018** `stat`, directory listings and row documents MUST not sniff/decode/download Blob content; raw-file size comes from the exact content descriptor.
**ORNA-VFS-019** Schema changes, MIME path changes and API writes MUST invalidate directory/attribute caches coherently. Existing handles MUST not retarget to a different row or reordered collection element.
**ORNA-VFS-020** Source files MAY be edited through the shared workspace source path; complete candidate validation and explicit grouped source edits are required when several files change together. No evaluation occurs during speculative file writes.

## Failure/recovery and boundaries

Default mount permissions are owner-only with `nodev,nosuid,noexec`; broader
access is explicit and never expands core authority. `allow_other` is not a
new row-level security model. Return EINVAL for malformed candidates, EAGAIN
for stale CAS, EROFS for snapshot writes, EPERM/EACCES for denied operations,
ENOSPC/EDQUOT for resource limits, and EIO for corrupt/unavailable backing I/O.
Exact Orna codes remain available through `orna mount status --json`.

**ORNA-VFS-021** Sync/rename acknowledgement, new content pins, local row commitment and namespace recovery MUST be journalled so crashes cannot lose acknowledged content.
**ORNA-VFS-022** FUSE writeback caching MUST be off initially; supported caching/IO modes MUST preserve per-open generation semantics despite non-VFS database writers.
**ORNA-VFS-023** The adapter MUST NOT expose a fake `.git` directory or writable raw store. Git commands target the backing repository, never generated row documents.
**ORNA-VFS-024** Unmount MUST refuse dirty/unresolved accepted-state recovery unless an explicit policy retains it safely. It MUST NOT report discarded drafts as saved.
