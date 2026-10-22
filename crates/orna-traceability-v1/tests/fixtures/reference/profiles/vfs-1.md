# VFS-1: exact initial host and path contract

## Host and selectors

Case-sensitive Linux FUSE; process permissions default owner-only. `nodev`,
`nosuid`, `noexec`; no `allow_other` unless explicitly authorized. Worktree
owner/generation checks happen at each mutation, not only at mount time.

Proposed/required CLI forms for the implementation:

```sh
orna --db ./library mount /mnt/library --cwd
orna --db ./library mount /mnt/old --at COMMIT_OR_REF
orna --db ./library mount status --json
orna --db ./library unmount /mnt/library
orna edit /mnt/library/Music/london/data.orna
orna edit --row /mnt/library/Music/london
```

`--cwd` is a workspace view and writable unless `--read-only`. `--at` resolves
once and is always read-only. Exactly one selector is required. Mountpoint
must be empty and must not contain/be an ancestor of the backing worktree or
its admin paths. Never recurse the mount while resolving store/source I/O.
A branch is not a mutable live mount selector. Use a separate ordinary Git
worktree for independent pending changes. A full export freezes a logical
snapshot instead of copying a live mount.

## Row/key directory mapping

Resolve namespaces/table names by stable identities in the pinned schema.
Project ordinary reachable source modules/resources and table directories;
source paths take priority on collision and the generated table component
escapes its first byte, not the schema name. Reserved root `.git` and `.orna`
are not writable/visible backing-administration directories in the mount.

For each logical key component, first use the language's canonical key text
(Str uses exact scalar sequence; Int shortest decimal; Decimal exact canonical;
UUID lowercase hyphenated; dates/times canonical; enum/reference per schema).
Composite keys use one component per declared key recursively; no free-form
slash splitting. UTF-8 bytes outside `[A-Za-z0-9._-]` become `%HH` uppercase
hex. Literal `%` and `~` are always escaped. Empty key is `~empty`; dot-only,
trailing-dot and reserved components escape relevant bytes. A literal
`~empty` therefore never aliases the empty key. Type/schema determines the
inverse; a text `1` and integer 1 in distinct table schemas cannot collide.

Reserved `data.orna`, `.git`, `.orna`, DOS device basenames and generated
namespace collisions escape the first otherwise literal byte. A Blob field
`data` with suffix `orna` projects as `%64ata.orna`, not the row document.
Only the exact projected canonical path is accepted; aliases cannot target
a different row/field. Directory case is preserved; no ASCII/Unicode casefold.

A component exceeding 200 encoded bytes uses `~key-<64 hex SHA256 of the
canonical typed key component>`; the complete directory address is resolved
through the snapshot-bound long-key secondary index using the same row store.
A full key is retained and verified. An observed digest collision fails with
`sys.vfs.name_collision` rather than merging records. No top-level alias file.
Mapping identity is schema/parent/key-position bound and updated atomically.
Long field names similarly use `~field-<full digest>` derived from stable field
path/type context. Linux NAME_MAX/PATH_MAX and API off_t bounds are checked;
unsupported deep projections report limits, never drop data. A later platform
may need a different named projection, not rewritten stored keys.

## Blob and nested field files

Top-level Blob fields use escaped field-name plus selected `.` suffix. The
media policy is MIME-1; suffix hints are stored with the value. Null option
produces no binary entry and must be represented explicitly as `null` in row
text. MIME change swaps the visible name atomically; old open handles remain
on the captured immutable content. Files do not gain execute bits from suffix.

Nested records use escaped field directories; tuple/list indices are decimal
positions under the captured containing-value version. Maps use typed encoded
keys; sets use full checked semantic digests. Any change to a containing
collection invalidates old writable baselines; a handle never retargets to a
new element at the same numeric position. Deep/large enumerations are lazy.
Large Str values over 64 KiB use the same projection machinery as a `.txt`
file and schema-directed `{text: "./field.txt", encoding: "utf-8"}` in
`data.orna`; writes must be valid UTF-8 and replace the Str, not create a Blob.
Non-Blob scalar text remains in the row document. No hidden Clob type appears.

`data.orna` is a complete stored-non-key-field document, not a patch. Missing
stored fields fail (including optional fields: use explicit null). Computed
fields and keys are omitted and cannot be supplied as stored replacements.
An unchanged descriptor uses the captured field, not a new path lookup into
current state. Path edits can select a compatible suffix for that field but
cannot attach another row/host file implicitly. Grouped import explicitly
supplies staged new content. Ordinary code strings never execute on read.

## Editable candidate and syscall rules

- `open(O_RDWR/O_WRONLY)` captures a whole-row semantic version and
  schema/source generation. `O_TRUNC` truncates only the private candidate.
- Known-target `write`, truncate and supported allocation update disk-backed
  extents; zero holes are defined bytes. Writer reads see its own candidate.
- `fsync/fdatasync` accepts one locked sequence through EDIT-1. Metadata
  required to preserve row validity is synced even for datasync.
- Scratch creation inside a row captures that row baseline. Scratch sync
  preserves bytes only. Rename onto target performs durable validated apply.
- Repeated `flush` attempts are idempotent. `release` never creates a commit.
- After sync accepted, later writes use a new candidate baseline; reads opened
  earlier are still pinned. Rejected candidates remain recoverable and quota
  accounted, with a unique diagnostic/draft ID in local state.
- No writable shared mmap, hard links, symlinks, device nodes, exchange/whiteout
  renames, or unchecked cross-table moves. Unsupported operations return an
  appropriate explicit errno. `copy_file_range` may implement the same draft
  writes or return EOPNOTSUPP for a userspace fallback; never fake success.
- chmod/chown cannot change database authority; access mode is host policy.
  Timestamps are view metadata, not automatic user columns. Updates never
  silently copy xattrs into application records.
- Required Blob unlink fails EPERM; optional unlink atomically assigns null
  subject to constraints. `data.orna` unlink fails. Row delete/rekey/create
  use explicit Orna mutation or the guarded grouped operation initially.
  Plain mkdir/rename of row directories does not invent schema/key semantics.
- Directory fsync flushes already admitted namespace journals, not all
  arbitrary dirty editor buffers. `fsync(temp)` is not row acceptance.

A writer can save legal but unwanted data; schema validation cannot infer
intent. Blind replacement after a late temp-create has no original-read
witness. Document that limitation; `orna edit` supplies a strong read baseline.
It stages outside live row state, displays validation failures, retains bad
edits, and atomically submits only after complete validation. `--row` groups
fields/content in one transaction when cross-field assertions require it.

## Read/cache/error guarantees

FUSE writeback cache is disabled initially. Use direct I/O or a tested mode
that preserves private drafts and per-open versions; small timeout/invalidation
alone is not proof of content isolation. Inode numbers identify retained
objects/handle generations, not a truncated row hash allowed to collide.
No cross-generation kernel page-cache sharing of incompatible contents.

`readdir` uses a generation-pinned cursor and bounded entries; lookup uses the
key/long-name index. `stat` reports descriptor size without media access.
Uncached missing backing objects cause EIO with detailed unavailable status,
not zero-filled bytes or EOF. Directory indexers/thumbnailers can trigger
broad real reads; their explicit requests count against budgets.

EINVAL: syntax/type/declaration mismatch. EAGAIN: stale expected version.
EROFS: historical/read-only write. EPERM/EACCES: authority/unsupported mutation.
ENOSPC/EDQUOT: capacity/quota. EIO: backing corruption/unavailability.
Keep stable Orna cause codes. Rejects must not be silently shown as successful
saves even though some applications ignore close errors.

Unmount refuses unresolved active drafts unless explicitly retaining them;
forced process death retains accepted-state journals and bounded recoverable
scratch. Never claim transient unfsynced writes survived a crash. Test actual
editors, dup/fork, temp rename, API writes, SIGKILL, disk-full and cache behavior
before claiming the full host profile.
