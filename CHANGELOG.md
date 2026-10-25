# Changelog

This file is the user-facing inventory for the OrnaDB source tree. It is not a
Debian changelog, a package manifest, or proof that a production artifact has
been published.

## Unreleased
- A project that imports `std` and `sys.Storage` now runs `use std.<module>.{name}`
  calls through the production authority route (`orna invoke`). The route is taken
  only when the project uses std and the admitted entry requires publication
  metadata; other std projects keep their previous path. The authority admits the
  captured standard dependency's function bodies, so a standard function is callable
  from the importing module instead of failing with `E2200` (bead `ornadb-5il1.430`).
  Glob imports (`use std.<module>.*`) bind the module's public exports; `use std.<module>
  as _` binds only the names the module records in its prelude; a named alias binds no
  names. A standard body calls its own module helpers, never a user function of the same name.
- Publication recovery re-verifies `.orna/store` payloads at the `IndexReconciled` stage before worktree materialisation, so a managed store edit made inside the `index` kill window is retained (E2300) and HEAD is not rewound. `publication_crash_matrix` gains a later-tail leg at every kill boundary (`prepared` included) and a managed-store kill-window leg (ornadb-5il1.406, ornadb-5il1.407).
- `orna serve` HTML pages (`/tree/`, `/blob/`) now send a
  `Content-Security-Policy` (`default-src 'none'`, inline styles only,
  `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY` and `Referrer-Policy: no-referrer` (ORNA-APP-012,
  ORNA-APP-013). Oversize bodies on non-`/orna` routes answer 413 instead of
  400, and the Origin/Host refusal now runs before the size check (bead
  ornadb-5il1.387).
- `orna mount status --json` now reports `writable`, `read_only`, `unresolved_drafts`
  (each `{path, detail}`), and `draft_error`. The record of a `--cwd` mount now
  stores `read_only false`, matching the FUSE session that was already writable;
  the record and status previously reported it read-only. Records written by this
  build carry a `read_only` value that older binaries reject as invalid (RecordInvalid).
  The `--read-only` flag from profiles/vfs-1.md:20 is not implemented; every `--cwd`
  mount is writable.
- `orna publish` gains four crash barriers behind the `crash-barriers` test
  feature: `ref-cas-pre-journal`, `store-worktree`, `worktree` and
  `runtime-complete`. `publication_crash_matrix` SIGKILLs the publisher at each
  barrier and asserts, after `orna publish` recovery: HEAD equals the base at the
  pre-CAS barriers (`ref-cas-pre-journal`, `store-worktree`) and equals the
  journaled candidate at the post-CAS barriers (`worktree`, `runtime-complete`)
  while the journal is present; the recovered candidate is checked to be parented
  on the captured HEAD only at the pre-CAS barriers; exactly one publication commit
  exists; a second publish is a no-op; staged and unstaged user work, pins, the
  runtime tail, the writer lease and locks are preserved or released; and
  `git fsck` passes. Managed-worktree and index reconciliation checks run only for
  non-store paths, and format-3 candidates have none, so they are vacuous here.
  No production behaviour changes.
- A complete-copy export of a workspace with a legacy format-1/2 snapshot now
  admits that snapshot by its recorded format record, and an offline restore
  re-admits each member (Gate F, `ACCEPTANCE.md:63-68`,
  `24-git-history.md:19`). Format-3 members still require their schema and
  store roots. Earlier builds refused every non-format-3 snapshot as a complete
  copy. The migrated-member leg (`migrate copy` publication) remains open under
  ornadb-5il1.192, and a gitlink whose dependency repository is absent is still
  refused.
- Runtime admission refuses a `HEAD` whose `.orna` metadata mixes layouts (a
  legacy `.orna/format.orna` or 1.0 sidecar beside `.orna/store`) with
  ORNA-REPO-CONTEXT-004 `MixedFormats`, naming `.orna/store`. It used to report
  ORNA-REPO-CONTEXT-003 (unknown format) because the gate reclassified the
  mixed snapshot (ORNA-META-001, ORNA-META-002; Gate A).
- `mkdir` and `rmdir` on the live writable mount now return `EPERM`, the same
  errno as every other refused mutation there (profiles/vfs-1.md errno table;
  41-vfs.md:103-105). They previously fell through to the fuser default, ENOSYS,
  which is not in the errno table. The `EXDEV` once considered for cross-directory
  rename is not in the reference spec, so it is not used; that rename keeps `EPERM`.
- One-shot `orna invoke` reads committed table rows at `HEAD`, binding each table to its committed identity (`10-execution.md`). A table-reading entry requires a committed project; without one it fails with `table reads need a committed project`. A mutating one-shot `invoke` fails closed with `table mutations`. `sys.rt.info` and `sys.Storage` are available inside table-reading entries. A one-shot read completes while another process holds the runtime owner lock, as a secondary participant. REPL expressions over committed table rows (for example `main.music | count()`) read the committed rows at `HEAD`.
- Known limit: a one-shot `orna invoke` in a multi-module project that declares a table fails closed with `E2200` (`table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run`). This wording is a spec-silent choice of this build. Any table-declaring module is refused, reached or not; the committed-table route admits a single module only. Extending one-shot reads to multi-module projects is tracked as follow-up work. Before this entry, the same projects were refused by the bounded evaluator with the generic `one-shot invocation requires an executable function-only project`; the outcome is unchanged, only the diagnostic is named.
- Known limit: a committed table read pages its relation scan with an ordinal cursor recomputed per page, so each page costs O(N log N) and a full scan with a one-row page size is O(N^2 log N). Carrying a keyed cursor is tracked as follow-up work.
- Known limit: a successful one-shot `orna invoke` prints only `invocation completed` and does not print the returned value. No one-shot invoke clause was located in the reference source (`reference/Orna-1.1.0/source/*.md`), so this output route is spec-silent in this build.
- A FUSE editor handle whose `close(2)` finds unsynced writes attempts the same
  acceptance as `fsync`, once (ORNA-VFS-008, `41-vfs.md:34-58`). A close whose
  baseline a peer has since accepted is refused: the peer's row stays and the
  candidate is kept in recovery storage (ORNA-VFS-009). Unmount refuses, rather
  than drops, a valid dirty draft that retention cannot keep, and a released
  handle does not re-admit a refusal that a later accepted save resolved.
  Earlier builds did not attempt acceptance at close and did not keep unsynced
  drafts on release.
- A FUSE write-intent open of a Blob projection (any write access mode, or
  `O_TRUNC`) now fails at open with `EPERM` and leaves the blob unchanged. It
  used to succeed or be ignored, and the later write failed with `EBADF`
  (profiles/vfs-1.md:104-106). Tools that write a Blob in place, such as `cp`
  onto `content.mp3` or an editor with `backupcopy=yes`, now see the error at
  open.
- The errno rule for the live writable mount is now uniform: every refused
  mutation there returns `EPERM` (41-vfs.md:103-105; profiles/vfs-1.md:139).
  This covers Blob write-open, Blob unlink and same-row rename, create in a
  table directory or in a row with a non-blob name, unlink of a row's
  `data.orna`, rename with flags, rename to a name other than the row's
  `data.orna` or Blob field, rename of a non-temporary node, and rename across
  rows. A cross-row rename is `EPERM`, not `EXDEV`: `EXDEV` would make `mv` and
  `cp` fall back to copy-then-unlink, which writes the bytes into the other row
  before the refused source unlink leaves the original behind. `EROFS` is
  reserved for the kernel read-only historical mount (`mount --at`), which never
  reaches these handlers. Rename with flags, rename of a row's `data.orna`, and
  path-truncate of a Blob are covered by `live_mount_structural_refusals_are_eperm`.
  Rename with flags uses `EPERM` where rename(2) would use `EINVAL`; `EPERM`
  follows the unsupported-mutation rule (41-vfs.md:103-105). Zero-length and
  sparse-hole Blob coverage remains open.
- A malformed format-3 native graph now reports a typed `GraphMalformed` cause
  under `ORNA-REPO-CONTEXT-017` when loaded through `load_row_map` for shape
  faults such as overlapping child ranges, broken counts, over-wide fanout, and
  a wrong schema domain. Operational graph failures (quota, cancellation,
  durability) are not reported as malformation.
- Runtime admission reads the format recorded at the committed `HEAD` before
  any worktree metadata or runtime state. A `HEAD` that records a format this
  build does not know, whether `database.orna` names another `repository_format`
  integer with any other fields or `format.orna` names one, is refused with
  `ORNA-REPO-CONTEXT-003` (ORNA-UPGRADE-001, ORNA-META-002). The refusal is a
  new `RuntimeError::UnknownHeadFormat`, and its persisted admin failure class
  is `unknown_head_format`. Other `HEAD` metadata failures keep the existing
  path.
- A format-3 snapshot that tracks a loose row file (an unreached
  `<table>/<key>.orna`) no longer loads with the row silently skipped:
  loading the committed snapshot or a commit candidate fails with
  `ORNA-REPO-CONTEXT-004` naming the file (ORNA-REPO-004, ORNA-UPGRADE-003).
  A module a `use` reaches under a table's directory is still a module.
  `orna commit` now also refuses a row file that `HEAD` already tracks, not
  only one the commit adds or changes. The CLI test fixtures no longer commit
  loose rows into format-3 repositories; row sources go straight into the
  store.
- Format-3 schema nodes record the Orna 1.1.0 schema identity, SHA-256 of
  `orna.schema.v3`, NUL and the closed storage descriptor, and every row map
  binds it. Earlier development builds recorded the descriptor's plain
  SHA-256, so the row maps of format-3 stores they wrote no longer open;
  re-create such a store with `orna init`, `orna commit` and `orna publish`.
  The repository API also gives each admitted row its `orna.row.v3`
  identity, which binds content and Blob annotations but no descriptor, page
  or overflow object ID.
- `orna publish` rewrites only the row pages a change touches. Maintenance
  reads the path to each changed key and the pages beside it, rewrites those
  pages and the branches above them, and keeps every other page by object ID
  without reading it; the result is the same root a complete rebuild gives.
  Earlier builds read and decoded every row of the table on each publish. A
  row written back with the value it already holds is no change, so such a
  publish reports nothing to publish.
- `orna publish` stores rows larger than a row page holds inline. A value
  whose stored encoding is over 8 KiB, or that holds more than 32 Blob
  references of its own, is written to the shared overflow graph (a kind-3
  node over an OGB-2 byte index, with a kind-7 index of the objects it
  references) and its row names it with tag 60113. Large fields are lowered
  first, so a row with one long text field keeps its other fields inline.
  The committed rekey log lowers its rows the same way, so a rekey between
  long keys stores each long address on its own instead of the whole row.
  `orna query`, `orna history`, mounts and the REPL read such rows back as
  ordinary values; earlier builds refused to publish them.
- Format-3 row maps and the relation map cut pages by the exact ORP-1 rule:
  the encoded node bytes, including the domain and entry-count heads, and the
  page's distinct native refs. The same rows now give the same root as the
  Orna 1.1.0 reference model. Earlier builds estimated sizes and summed refs,
  so they cut pages early for large rows and for rows sharing Blob
  descriptors.
- A format-3 project whose `main.orna` is larger than 64 KiB can query,
  publish, show history and mount its rows. Opening a row map pins the
  committed `main.orna` by its blob OID instead of reading the source under a
  64 KiB cap that store format 3 sets only for `database.orna`.
- `orna mv OLD NEW` works in a database made by `orna init` once its first
  `orna commit` exists. Like `git mv`, it stages the rename and commits
  nothing. The table's declaration is renamed in the worktree module and in
  its staged copy, and the store's identity inventory is restaged in the same
  index update, so the table and its fields keep their IDs. Other unstaged
  edits to the module stay unstaged. `orna commit` right after `mv` keeps the
  table's relation and its published rows (ORNA-OBJECT-003, ORNA-CLI-001).
  `orna mv` no longer reads or writes `.orna/object-identities.cbor` and no
  longer moves loose-row directories, which format 3 does not have. Before the
  first commit it refuses with E2101 because no identity is staged yet. If the
  index update is refused, the source edit is rolled back. The Rust API adds
  `Repository::stage_object_identities`,
  `RelationAdmissionError::SourceNotStaged` and
  `ObjectIdentityManifest::with_renames`.
- Deleted the old-stack island crates `orna-client`, `orna-artifact`, `orna-protocol`, `orna-standard` and `orna-runtime-tty` (deletion programme step 7). None had a production caller: their only remaining dependents were the `orna-application-v1` dev-dependency (now removed) and each other. The `orna-standard` snapshot oracle `v1_profile.rs` is ported to `crates/orna-evaluator-v1/tests/standard_profile_v1.rs` (imports rewired to `orna-standard-sources`, fixtures moved alongside); `orna-standard-sources` remains the canonical source. The `just` recipes for the removed demos, Qt runtime and Studio shell (`runtime-tty-demo`, `client-*-demo`, `demo-suite`, `runtime-qt-*`, `studio-qt-*`, `runtime-suite`, `runtime-display-suite`, `runtime-abi-header-check`, `runtime-abi-parity`) were removed with them.
- The `orna` CLI no longer links `orna-client` (and its HTTP, WebSocket and Qt-ABI dependencies) for the `:watch` types. `live_presentation` moved unchanged from `orna-client` into `orna-live-v1`, which the CLI already depends on (deletion programme step 4). The public API of `orna-client` no longer exports `live_presentation`; import it from `orna-live-v1` instead.
- A committed source tree whose walk would visit more than 2^20 entries (every interior tree and every listed entry, counting each occurrence of a shared subtree), list more than 2^27 path bytes, or nest deeper than 256 levels is refused before git expands it. A project loaded from such a snapshot reports `ProjectLoadError::SourceTreeLimit` (CLI E2100, titled as a walk-limit breach) instead of spending unbounded time and memory on the expansion (W46 F1).
- Managed paths refuse a `.git` component at any depth and in any letter case,
  including the Windows and macOS spellings Git refuses (trailing dot or space,
  `git~1`, a `:` stream suffix and HFS ignorable characters), on write and when
  a committed tree is loaded or listed. Before, only an exact `.git` first
  component was refused. Path length is not capped, because Git stores names of
  any length. Untracked user paths such as `Sub/.GIT/notes` still appear in
  status and checkout plans, and a forced checkout leaves them in place.
  `orna export --check` and `--restore` refuse an archive whose dependency path
  names Git metadata before anything is restored. Managed paths also refuse a
  backslash, a NUL byte, an empty component (`a//b`, `a/`), and `..`, absolute
  and other non-normal components, on write, in publication and checkout
  journals, and in compact manifests. The system worktree listing skips a
  Git-metadata-named entry (`.git:x`, `a\.git`, `.git.`) instead of reporting it
  as a user path.
- `scripts/orna_extract.py` is an independent extractor. It reads a format-3
  repository's Git objects at a commit through `git cat-file --batch`, without
  the engine's readers, and writes the modules, the schema, the rows and each
  Blob's bytes. It refuses any tracked `.orna/` entry other than `database.orna`
  and `store` (ORNA-META-001).
- Work ADRs that Orna-1.1.0 supersedes, or that no longer constrain code, are
  archived under `docs/decisions/archive/1.0/` with an evidence table in
  `STATUS.md`. Duplicate ADR numbers are renumbered: the offline LSP ADR is
  0119 (was 0059) and the CLIENT VM trust boundary ADR is 0120 (was 0101).
- The live WebSocket handshake no longer accepts URL-alphabet or mixed-alphabet `Sec-WebSocket-Key` values; only padded standard base64 is accepted (RFC 6455).
- `orna --help` names the command `orna`. It says that `unmount DIR` detaches
  the kernel mount, including an orphaned Orna mount, and releases its view
  record. `annotate` help no longer names a wire encoding (ORNA-UX-002). The
  `migrate copy` help says that `--no-persistent-consumers` confirms that
  consumer checkpoints are carried, not reset (ORNA-UPGRADE-008).
- The Linux package archive holds only `usr/bin/orna` and its distribution
  manifest. The retired embedded-engine manifest, its builder image and its
  predecessor-engine fields are gone, so a default `packaging/linux/package.sh
  build` no longer needs an input that nothing produces.
- `orna commit` stages the format-3 store: a database made by `orna init`
  gets its `.orna/store` on the first commit, which creates the branch on an
  unborn HEAD. Each newly declared table is admitted with its schema node, an
  empty row map and a relation entry. Stable object IDs live in the store as a
  sealed identity relation keyed by UUID rather than
  `.orna/object-identities.cbor`, which is read only from older commits. A
  commit that fails leaves its newly staged tables free to change or drop, and
  `orna commit` refuses to stage while an interrupted publication awaits
  recovery. `orna publish` refuses rows whose table has no relation in the
  committed store. Identity rows record each object's kind as the
  specification's `sys.ObjectKind` ordinal (table 2, field 3, unit 15, type 8,
  variant 10, type and variant fields 9), not a private integer. Stores written
  with the private integers are not readable, and the sidecar's version 1 codes
  are mapped only when reading an older commit.
- `orna commit` refuses a staged index whose `.orna/store` is partial after `git rm --cached .orna/store`, leaving skip-worktree payload entries behind, with `E2302` naming the unstaged committed relation and its `git restore --staged .orna/store` remedy, instead of a generic staging failure.
- A live eval or event whose CWD snapshot pin the runtime cannot resolve is answered with a correlated `wire.snapshot_expired` diagnostic before any application effect runs. The pin is never rebound to the current generation.
- A live `cancel` whose target names no admitted request now acknowledges with a correlated Unit result and keeps the connection open. Before, the host answered `Denied` and dropped the socket. The application is not invoked for an unknown target, in memory or durable.
- `orna serve` hosts the clone's durable runtime. A live eval that inserts
  into a project table commits the row together with its terminal outcome,
  and resending the same request ID returns the recorded outcome without
  running again; before, every writing eval was refused. Each eval is one
  REPL input: a declaration, binding or import answers with typed Unit and
  persists for the session, and an input that declares a function named
  `serve` or `main` is an ordinary declaration rather than a one-shot module
  run. An explicit resync is answered with the watch's latest full snapshot
  under its own request ID; before, a changed value produced a delta with no
  request ID.
- `orna invoke` attaches the runtime host context before it evaluates, so `sys.rt.info()` completes in a project and fails closed outside one. The record is computed once per attachment; an incompatible system version reports `sys.version.incompatible`, an unreadable standard snapshot pin reports `sys.snapshot.incomplete`, and the build identity reads git HEAD only from the crate's own repository, otherwise `+nogit`.
- Semantic analysis keeps described `sys` member types nominal: `checkpoint.position` is a `sys.CheckpointPosition` that matches annotations and `sys.admin` signatures, and `storage.publication_policy` stays the opaque `sys.Value`.
- A qualified `std.collection` `sum`, `min` or `max` call is checked against a monomorphic pinned export's element type and takes its declared result.
- A table key whose type annotation does not resolve reports only the unresolved name, without a second primary-key encoding error.
- Enum values can be written in source: `Level.low` references a payload-free
  variant and `SyncResult.ok { count: 3 }` constructs a payload variant, also
  through named imports and module bindings. `orna check` reports an unknown
  variant as unresolved and unknown, missing and mistyped payload fields as
  `E2003`, `E2002` and `E2001`. The project REPL and `orna run` evaluate the
  constructed values, so `case` matches them and tables store them.
- `std.io.fs` operations resolve every path from the root directory handle held
  since the host allowed it, one component at a time without following
  symbolic links, and act on the final entry through its open parent directory.
  Parent traversal, absolute paths, symbolic links and `.git` metadata are
  refused as `sys.host.filesystem.denied`, while a missing target reports
  `sys.host.filesystem.not_found`; only `symlink_metadata` still describes a
  final link, without following it. Writes without overwrite create
  exclusively, moves without overwrite never replace a destination, and
  listings omit `.git`. Platforms other than Linux refuse these operations as
  unavailable.
- `sys.explain` works in `orna run`, `orna serve` and the REPL. Each host pins
  it to its activation snapshot: the admitted project tables, plus
  `sys.Storage` and `sys.MaintenanceJob` where the host serves their rows
  (`orna run` and `orna serve`; the REPL serves neither yet). A query returns its
  structured `sys.Plan`. A `sys.Diagnostic` returns a `sys.Explanation`: the
  row's own message as the summary (the explicit marker only when the row is
  redacted), its causes, its help as suggestions and the objects it names.
  `sys.explain(FunctionRef)` is dispatched. The production explain catalogue
  does not resolve function references (`resolves_functions` defaults to
  false), so every function reference reports `sys.version.incompatible`.
  `sys.object.not_found` applies to an unknown source, or to an unknown function
  only when the catalogue resolves functions.
- Changed failure codes, all now taken from the portable failure table:
  - `sys.explain`: `sys.abi.unavailable` becomes `sys.version.incompatible`.
    `sys.abi.provider_failed` becomes `sys.object.not_found` for an unknown
    source, `sys.version.incompatible` for a stage or source the
    planner cannot represent, and `sys.object.wrong_kind` for a reference to
    another relation. `ORNA-EVAL-TYPE` no longer rejects function and
    diagnostic arguments.
  - `sys.blob.capture_file`: `ORNA-INGEST-001`, `-002` and `-003` become
    `sys.blob.source_denied`; `ORNA-INGEST-004` becomes `sys.blob.limit`; and
    `ORNA-INGEST-005` becomes `sys.blob.source_changed`. A negative or
    unrepresentable `max_bytes` is `sys.blob.bounds`, and a repository capture
    quota is `sys.blob.limit`. A host without a filesystem provider reports
    `sys.blob.source_denied`, and one without a repository Blob writer reports
    `sys.version.incompatible`, instead of `ORNA-EVAL-UNSUPPORTED`. The native
    host operation declares `sys.blob.source_denied`, `sys.blob.bounds`,
    `sys.blob.limit` and `sys.blob.source_changed` instead of
    `sys.blob.capture.*`; the binding raises only these declared codes and the
    global `sys.version.incompatible`, and the portable operation declares the
    same set plus that global code, which every callable operation carries.
    `orna annotate` source errors cite the portable code.
  - `sys.blob.annotate`: an OVB-1 value is
    `sys.codec.unsupported_value_profile`; arguments that are not annotated
    Blobs are `ORNA-EVAL-TYPE`; a host without the generated provider is
    `sys.version.incompatible`. No `sys.abi.*` code reaches programs.
  - `orna serve` stream administration: `sys.admin.unavailable` becomes
    `sys.admin.missing_object`, `sys.admin.busy`, the specific checkpoint or
    snapshot code, or `sys.storage.unavailable`. A dispatcher without resume
    reports `sys.admin.read_only`, unavailable publication metadata reports
    `sys.storage.unavailable` instead of `sys.storage.metadata_unavailable`, and
    `sys.admin.result` is gone.
- Every portable `sys` operation now declares its failure codes explicitly
  through `#[ornasys(failures = ...)]`, replacing the namespace-prefix guess.
  The build checks that every published code is declared by an operation, is
  global, or belongs to a non-callable surface (the `sys.repl` singleton or the
  VFS).
- A failed stream administration call (pause, resume or checkpoint reset)
  records a stable failure class in its invocation audit row, with the
  generation its writer fence observed. A retry under the same invocation ID
  returns the recorded error instead of a storage-unavailable error. A stale
  capture stores the capture that rejected the call and replays that same
  error, even after later commits advance the CWD. A rejected stream-reference
  resume now records its failure like a rejected pause.
- When the CWD generation cannot be read, the row records no generation (NULL)
  with a `failure:unobservable:<class>` outcome, and the first call still
  returns its original error. A retry replays `<class>` as the same error when
  the class is payload-free. The payload-carrying classes `stale_capture`,
  `fault_injected` and `legacy_head_read_only`, and a bare legacy
  `failure:unobservable` row, replay as `RecoveryInvalid`, because their
  payload was never stored. The observed generation is nullable in the audit
  schema, migrated in place for existing stores; unobservable rows are
  excluded from the checkpoint-reset generation ordering.
- A skip lease refused because the
  failure is no longer open is `sys.failure.state_conflict`, not
  `sys.failure.skip_unsupported`, and `orna_runtime_v1::failure_transition_error`
  no longer takes a lease purpose.
- Removed the unreachable duplicate explain implementations:
  `orna_sys_v1::ExplainQueryProvider`, `QueryPlanResolver` and their constants,
  `orna_sys_v1::explain_diagnostic` with `Explanation` and
  `DiagnosticExplanationError`, and the `orna_foundation_v1` explanation
  envelope (`SystemDiagnosticExplanationInput`,
  `SystemDiagnosticExplanationOutput`, `system_diagnostic_explanation_input`
  and `SystemDiagnosticAdapterError`). Also removed
  `SysHostBindingRegistry::with_explain_catalogue` and
  `CaptureFileError::code`.
- Removed the unreachable `orna-core` catalogue bridge from `orna-runtime-v1`:
  `CatalogueAdmission::from_artifact`,
  `RuntimeState::commit_validated_catalogue_table_request_activation_with_revision_pair`
  and `RuntimeState::catalogue_revision_pair_at`. Nothing called them, so every
  catalogue admission already recorded no revision pair. `orna-runtime-v1` no
  longer depends on `orna-core`. The `runtime_catalogue_revision_binding` table
  stays in the state schema, so existing `state.db` files open unchanged.
- Opening a repository format context reads only the root tree, the `.orna`
  tree and its metadata blobs, through one long-lived `git cat-file
  --batch-command` reader per repository handle. It no longer lists the whole
  commit, `.orna/store` included, with a Git process per entry, so point reads
  no longer slow down as the store grows, and a snapshot with more than 65,536
  tree entries opens.
- The project loader's 4,096-entry limit is now `ProjectLimits::max_source_modules`
  (formerly `max_repository_entries`) and counts source modules only. The
  loader never walks `.orna/store`, in the worktree or in a committed snapshot,
  so `orna check`, `orna run` and `orna repl` load databases with large stores.
- The evaluator's depth limit (`Limits::max_depth`, default 64) now counts
  active calls: function, closure and callback bodies. Expression nesting inside
  one body no longer consumes it, so recursion depth no longer shrinks as bodies
  nest deeper. The parser bounds expression nesting. Pattern and decoded-value
  nesting are checked from their own roots against the same limit. Exceeding
  any of these fails with `ORNA-EVAL-LIMIT` and never returns a partial result
  (ORNA-CLOSURE-004).
- An assignment inside a `case` arm now replaces the enclosing `let` slot, as
  assignments inside blocks and loops already did (ORNA-CFLOW-005). Names bound
  by the arm pattern stay local to the arm.
- Counting an in-memory list costs one evaluator step instead of one step per
  element. A plain `=` assignment no longer copies the value it replaces.
  Callback calls no longer copy the callee's syntax tree. Block, loop and `case`
  write-back moves values instead of copying them.
- Evaluator list values share their elements between copies. Appending to the
  newest list reuses spare capacity, so building a list one element at a time
  costs amortised constant work per element instead of copying the whole list
  each time. `union` and `__list_concat` charge steps only for the elements
  they actually write.
- Ordered comparisons (`<`, `<=`, `>`, `>=`) between two Decimals evaluate
  exactly across scales (ORNA-COMPARE-002). They previously failed with
  `ORNA-EVAL-UNSUPPORTED`.
- Decimal `%` evaluates as the exact truncated remainder, matching Int `%`: the
  result keeps the dividend's sign. It previously failed with
  `ORNA-EVAL-UNSUPPORTED`.
- Exponent-only Decimal numerals such as `1e3`, `1E-2` and `2e-30` evaluate as
  exact Decimals (ORNA-LIT-002). They previously failed with `ORNA-EVAL-VALUE`.
- Relation scans charge each streamed row to the step budget. Only values that a
  stage or terminal keeps (sort buffers, distinct sets, collected results) count
  against `max_collection_items`. `count`, `exists`, `every`, filtered or
  dropped counts, `sum`, `min` and `max` now work on relations larger than the
  item budget. Sorting more rows than the budget still fails with
  `ORNA-EVAL-LIMIT`.
- `sort_by`, `rank` and relation sorts use a stable merge sort, with
  O(n log n) comparisons that each cost one step, instead of an insertion sort.
  The default evaluator step budget is now 200 steps per item plus 10,000
  (419,600), down from 2,516,752. The pinned std helpers stay within it at the
  2,048-item limit. A runaway loop such as `while true {}` now fails with
  `ORNA-EVAL-LIMIT` six times sooner. Std bodies that are inherently quadratic,
  such as the Orna `std.collection.distinct`, reach the limit at a few hundred
  elements.
- `std.collection.map` evaluates its pinned Orna while-loop body instead of the Rust binding when the pinned `map_from` is a loop. Historical pins whose `map_from` is recursive keep the native binding. Other list-input collection operations already take their Orna bodies; relation inputs stay on the Rust relation path.
- Format-3 transaction validation maps positional row values by stable field-ID order before evaluating row assertions.
- `orna serve` no longer installs a host-generated devtools table browser; local query, mount, and content workflows remain independent.
- Remounted writable workspace views capture one runtime activation and project
  current row values only when table identity, exact key, and ordered stored
  field layout match. Rekeys without row-lineage metadata stay pinned; Blob
  bindings remain pinned.
- Blob annotation binding canonicalizes caller-supplied MIME-1 values (including
  the preferred suffix) before storing the canonical wire annotation.
- JSON encoding supports Money values when their registered nominal Currency metadata is available. It preserves the exact decimal amount as a string and emits the registered currency code.
- Semantic quantities retain reduced dimension exponents, exact rational scale,
  and affine metadata through numeric aggregates. Integrating speed by elapsed
  time adds the Time exponent and returns a length quantity. Dimension expressions
  parse qualified names without consuming multiplication or division operators.

- `orna publish` refuses a format-1/2 `HEAD` before recovery, runtime access or
  any freeze, with `E2300`; it writes nothing and the pending range stays. The
  help says this build has no writer or in-place upgrade for format-1/2
  snapshots and names the upgrade work item, ornadb-5il1.184. Inside a prepared
  migration copy it says the format-3 candidate is prepared but not activated,
  and that activation is tracked as ornadb-5il1.192. The loose and compact
  (Parquet) row writers, their recovery paths and the CLI compact-journal
  recovery are retired. Format-1/2 commits keep their library readers, but no
  CLI verb reads them yet (ORNA-UPGRADE-003, ORNA-ROW-013).

- The `format.orna` records that the legacy writers produced are now read as
  read-only format-1/2 inputs: OrnaDB 1.0's `{repository_format: 1,
  storage_profile: "compact-storage-v1"}` and the previous draft's closed
  format-2 record. They were rejected as malformed, so `orna migrate copy`
  failed on every real legacy repository. Runtime writes now follow the format
  recorded at `HEAD`. A format-1/2 `HEAD` refuses them with
  `ORNA-REPO-CONTEXT-015` even when the worktree holds format-3 metadata, and
  `orna run` and the other write verbs exit with `E2200`. The one exception is
  a prepared migration copy (ORNA-UPGRADE-001).

- `orna migrate copy` reports a source whose format context cannot be admitted as `E2100` titled `migration source could not be admitted: <cause>`, with the cause's `ORNA-REPO-CONTEXT-*` code, and creates no destination.

- `orna run` and the streaming project invocation keep the underlying failure on `E2200`. A runtime error carries its `sys.*` public code when it has one, otherwise its non-disclosing display text; a legacy read-only head refusal carries its `ORNA-REPO-CONTEXT-*` code and names its remedy in the help line. A streaming failure carries the runtime stream-failure label and no evaluator nested causes. Nested cause codes apply to `orna run` only: an evaluator rejection lists each nested cause code the evaluator itself emits, and redacts any other code, including a user-authored `ORNA-*` code. These details render with `--debug` only, per ORNA-UX-004. Evaluator spans and the shared source diagnostic remain open under ornadb-5il1.152 and ornadb-5il1.101.

- Format-3 store-only publication validates canonical row keys without legacy
  loose-row path resolution, including no-op preparation.

- Explicit format-3 row rekeys retain their relation identity and canonical old/new
  addresses in the publication journal so replay and recovery do not infer a move
  from filenames or collapse it into delete-plus-insert.

- Missing unrooted prepared publication commits can be discarded during recovery
  only when HEAD and the base index still match, preserving frozen runtime
  mutations for candidate rebuild and retry after Git garbage collection.

- Publication reconciliation keeps bounded OGS node metadata materialized while
  leaving OGB-2 raw chunks in Git's object store, avoiding a second worktree copy
  of imported media.

- Citation scanning counts active test operands and referenced test helpers/constants while excluding diagnostic text, format placeholders, unused helpers/constants, and non-test literals. The optional `--locations-output` flag emits deterministic per-ID source locations.

- Replacing a mounted top-level Blob file updates that field's immutable
  descriptor through the durable row-save transaction while retaining the
  existing row key and the previous content for already-open handles.
- The CLI `add --row TABLE KEY` stages one selected logical row into the index. `commit -m MESSAGE` reviews that exact staged candidate and publishes it through the protected reviewed-candidate repository path; runtime-tail publication remains the separate `publish` command.
- Source identity reconciliation accepts explicit rename mappings and preserves
  existing IDs across authorized name changes. Unassisted renames remain
  delete-plus-create, and retired IDs are never reused.
- LSP declaration rename identity follows only WorkspaceEdits with exact resulting
  document text observed at a newer didChange version; saving requires disk text
  to match current document text. If one didChange combines the returned edit with
  unrelated edits so the exact result is not observed, that step is not persisted
  (fail closed); reusing a renamed-away name before save may lose identity continuity.
- The `mv` CLI command renames committed tables and retains their table and
  child-field IDs in uncommitted source and identity-manifest changes. It can
  resume after a source rename is interrupted before the manifest update. The
  format-3 store keeps rows under the retained table ID, so `mv` moves no row
  files. If a legacy format-1/2 loose-row directory exists, `mv` still renames
  it. That is legacy-layout handling, not a format-3 feature: Orna 1.1.0
  forbids loose-row writes for the new format (ORNA-UPGRADE-003).

- Optional Blob fields preserve `Some` bindings through source-row admission,
  metadata queries, history diffs and offline export; overflow verification counts
  nested Blob references through `Some` without reading payload bytes.
- FUSE workspace mounts page through the pinned row index on listing, resolve
  known keys by the index, and isolate projection errors as EIO so one corrupt
  row does not block neighboring rows.
- FUSE workspace mounts hold a private, per-worktree reference to their pinned
  snapshot so reads survive HEAD changes and garbage collection. SIGINT,
  SIGTERM, and SIGHUP clean up mounts, while orna unmount DIR can detach
  orphaned mounts when DIR is relative to the command's current working
  directory.
- `mount DIR --at SELECTOR` now starts a read-only FUSE session pinned to the
  resolved commit, so its mounted paths continue to read that historical state
  after workspace HEAD advances.
- FUSE teardown serializes daemon and CLI cleanup so concurrent detach paths do
  not race to remove the same view pin and record.
- The trusted REPL exposes the root namespace as `main` unless a user module named
  `main` exists, and admits `main.add` Blob rows using typed keys against the
  committed format-3 schema. A semicolon-terminated capture/add submission
  suppresses the row result while retaining one durable runtime transaction.
  Captured scratch refs share the runtime owner and are reclaimed when a later
  runtime reopen recovers a crashed session.

- Decimal division preserves exact terminating quotients and provides explicit
  scale with half-even rounding.

- Decimal currency suffixes construct Money only when the selected nominal
  implements Currency; a bare uppercase code is no longer sufficient.

- `Money<C>` requires a currency type: a nominal type that implements the core
  `Currency` protocol, or a generic parameter bounded by it. A declared or
  imported `Currency` counts only when its surface is exactly the core one.
  `Money<Int>`, table rows, types without `Currency` and undeclared names are
  rejected, and three-letter names are no longer assumed to be currencies.
  Imported currencies now construct Money from `12.34.GBP`. A Float currency
  literal such as `12.34f.GBP` is rejected with E5004.


- Format-3 table schemas encode defaulted fields and keys as stored or key
  fields, computed fields as non-stored role-2 selectors with an expression
  digest, table-typed fields as stored references, and enum, nominal record,
  refined, transparent-alias and `Money` field types with their closed
  definitions. The identity manifest issues stable IDs for nominal types, enum
  variants and their fields. Explicit type and variant renames passed to the
  manifest reconciliation API carry those IDs; `orna mv` and LSP rename still
  rename only tables and fields. A manifest that records these new identity
  kinds cannot be read by older binaries, which reject unknown kinds.
- Format-3 rows admit every registered value tag. Tables keyed by Uuid, Date,
  Instant, Decimal, enum, reference or tuple values store and order rows by
  logical key, and Int and UInt spellings of one integer are one key.
- MIME and file-name code reads one MIME-1 registry pinned to the Orna 1.1.0
  `profiles/media-1.json`. Octet-stream and unregistered media types accept any
  safe suffix hint, and stored Blob annotations with quoted parameters or `_`
  hints read back. `sys.blob.capture_file` now supplies the
  `application/octet-stream` annotation with no suffix instead of guessing from
  the file extension; declare the media type with `sys.blob.annotate`.

- The parser no longer changes valid source silently: a comparison followed
  later by `> (` keeps all of its tokens; a braced control statement (`if`,
  `case`, `for`, `while` or `loop`) ends before a following `(`, `[`, `-`, `+`
  or `..` item; control headers never read `Name {` as a constructor (an
  unparenthesised one before the body is reported); table subjectless
  predicates keep their operator; and spaceless arithmetic such as
  `1+10-20-30` is not a date.
- Case guards such as `x if x < limit:`, `assert` as a variant, label, field or
  member name, and default parameters of generic functions parse in modules as
  in the REPL. Legacy assertion, colon-bound and assignment-expression
  diagnostics now come from the failing production in every entry point.
- Publication refuses a detached HEAD, a stale reviewed candidate, managed-file
  edits and store conflicts before it writes its prepared journal, and rechecks
  the captured index immediately before the ref compare-and-set. A refused or
  lost compare-and-set discards that journal, so the pending batch can be
  retried. Recovery likewise discards a prepared journal when neither HEAD nor
  any local branch contains its candidate, even if the candidate object
  survives or the index changed since; legacy-migration journals stay
  resumable. Recovery also recognises a reconciled `.orna/store` index
  installed before its journal stage. New candidate objects are flushed before
  the ref names them, and the ref is flushed before the runtime tail is
  consumed.

- `orna serve` no longer holds its shared session state while a WebSocket
  waits on its peer, so open sockets do not block other sockets or
  `/orna/session` requests. A resume replaces and closes the old socket, idle
  sockets are pinged and silent peers are disconnected, and request heads are
  bounded in time and size. Requests whose Host is not the listener's loopback
  authority, and cross-origin writes, get 403. Smart-HTTP push now requires
  `serve --allow-push`.

- `orna serve` ends live sessions in order. `DELETE /orna/session/<id>` cancels
  and joins the session's application work and closes its socket before it
  answers 204, including while the socket is attached; it used to answer 503
  then, and 400 once the session had evaluated anything. A sweep ends sessions
  whose lease has elapsed in the same way.

- `orna serve` serves at most 64 connections at once. The accept loop answers
  any further connection with 503 straight away, without reading its request.

- `orna serve` bounds each HTTP request body and each response with one
  deadline for the whole transfer, 15 s plus the time it takes at 64 KiB/s,
  instead of bounding each blocked read or write. A peer that sends or reads a
  few bytes at a time can no longer keep a connection busy.

- Live session HTTP errors use the statuses `30-protocol.md` assigns: a
  WebSocket upgrade from another origin gets 403 instead of 410, and a DELETE
  carrying a token other than the live session's current one gets 401 instead
  of 410. A child-free DELETE that would first have to drain application work
  gets 503 instead of 400. `Content-Type: application/json; charset=utf-8` is
  accepted.
- Semantic checking types `+ - * / % ^`, unary signs and compound
  assignment through one operator table: Int, Decimal and Float never mix,
  Money scales only by exact numbers in one currency, declared-unit
  quantities follow dimension and affine algebra, and other operand pairs
  report `E2001` (`E5001` for incompatible dimensions). Every reachable
  `return`, including inside anonymous functions, is checked against the
  declared result or joined into the inferred one. Blocks without a final
  expression, bare `return;`, bare `break` and `T.delete` have type `Unit`.
- `sys.rt.info()` is linked into the production sys provider, so it evaluates
  to a `sys.RuntimeInfo` value carrying the runtime's real coordinates instead
  of an unlinked callable. Admission no longer reports a version diagnostic for
  it (ORNA-SYS-006).
- The CLI's `migrate abort DEST` discards a prepared format-3 migration copy only
  after verifying that DEST is the copy prepared from the current source. The
  source is unchanged. Until the verified copy is renamed to its reserved
  sibling quarantine, a change to the source commit, index, or working tree
  since preparation makes the command refuse and remove nothing. That rename is
  the commit point: a repeated `migrate abort DEST` finishes an interrupted
  removal without rechecking the source, reports nothing to discard once nothing
  remains, and `migrate copy` refuses the destination while the quarantine
  exists.
- The command-line executable is now `orna`; the package and crate remain
  `orna-cli-v1`. The former `orna-cli-v1` executable name is removed with no
  alias or shim. Build output is `target/<profile>/orna`, and the Linux package
  and release paths, CI, and the command examples in the README and docs name
  `orna`.
- `TransactionalEvaluator` and `DurableTransactionalEvaluator` now admit
  transactions against the production catalogue by default, so a user module
  whose path matches a conformance fixture namespace, such as
  `vehicle/corsa.orna`, is no longer rejected with `ORNA-S002-NAMESPACE`. The
  conformance fixture catalogue is an explicit harness opt-in only, through
  `TransactionalEvaluator::with_fixture_catalogue`.

## 1.0.0 — release inventory (publication pending)

> **Historical inventory (note added 2026-10-21).** This section was written
> for an Orna 1.0.0 release that was never published. Most surfaces that it
> lists were removed from the tree before any release. Do not read it as a
> description of the current product. The current target is Orna 1.1.0
> (publication `final-2026-10-05`, repository writer format 3). Its reference
> bundle `../reference/Orna-1.1.0` has 947 requirements. The Unreleased section
> above describes current behaviour.
>
> These surfaces were removed. No release shipped them.
>
> - The previous `orna` binary of the `orna-server` package. It provided
>   `orna source check`, `source diff` and `source apply`, daemon and server
>   administration, runtime description, raw calls, USER state, inspection,
>   security administration, the Inspector, `--explain`, argument files,
>   canonical output, the JSON, table and CSV presenters, invocation tracing
>   and the managed Unix-socket `invoke` route. Commit `4777f6213` removed it
>   (issue #603). The current `orna` command is the `orna-cli-v1` binary. It
>   rejects `orna+unix://` and TLS endpoints.
> - The direct SQLite adapter that a filesystem `--db` path selected. Commits
>   `b0f4c4f76` (issue #285) and `3dc8360b5` (issue #603) removed it. `--db
>   PATH` now selects a local Git worktree. SQLite remains only as private
>   runtime state.
> - The PostgreSQL engine, its generated SQL migrations and the `_orna_kernel`
>   tables. Commits `3dc8360b5` (issue #603) and `f1fb5684c` (issue #889)
>   removed them.
> - The accepted demo inventory `examples/accepted-demos.toml`. Commit
>   `d0dbd7a59` removed it.
> - ORV5 and ORV6 as value contracts. Orna 1.1.0 defines OVB-1, OVB-2, ROV-3
>   and SOV-3. The CLI does not depend on the `orna-runtime-tty` crate.
>
> Four bullets below describe surfaces that are still in the tree. They were
> corrected in place on 2026-10-21: `mount --at`, the system API publication
> fields, `migrate copy`, and loose-row paths.

**Status:** The Cargo workspace currently identifies itself as `1.0.0`, but this
checkout is not a published production release. The audited immutable
normative reference bundle is maintained outside this checkout at
`../reference/Orna-1.0.0`; its requirement ledger still records all 870
requirements as not executed. This inventory must not be read as a conformance
claim.

The accepted release-mechanics decision reserves a future Debian 12 amd64
release identity of `orna 1.0.0-1` with a signed `v1.0.0` source tag. That
decision does not declare the product baseline complete. This checkout now has
a minimal `packaging/linux/` provenance/install artifact recipe and focused
tests, but it has no Debian changelog, signed artifact, or clean-host
production-package evidence.
See the [release-mechanics decision](docs/decisions/archive/1.0/0047-first-one-zero-release.md)
for the required release authority and the
[maintainer runbook](docs/maintainer-runbook.md) for evidence rules.

### Product boundary

- OrnaDB is a backend-neutral `.orna` source language and typed application
  model. A source file does not select a backend dialect, and the public product
  does not ask users to write backend-specific SQL.
- A filesystem database path selects the direct SQLite adapter. That adapter is
  a bounded implementation/runtime route for the same source model; it does
  not define a second source language. Physical operations the adapter cannot
  represent are rejected rather than given backend-specific semantics.
- `orna://` and `orna+unix://` endpoint forms are parsed. The bounded installed
  `invoke` route supports only the current managed Orna Unix socket through
  local authentication; remote TLS and other explicit-endpoint commands remain
  unavailable in this checkout and fail closed.

### Implemented source and server surface

The following behaviors are present in the source tree and are exercised by
checked-in unit, integration, or accepted-fixture coverage. The list describes
implementation boundaries, not a claim that every environment-dependent
release gate has passed.

- `orna source check <file.orna>` checks one regular UTF-8 source file without a
  database, network access, configuration, child process, or write. It is a
  standalone check against the empty application catalogue; it does not prove
  continuity with an installed revision.
- `orna source diff <file.orna>` prepares a read-only semantic comparison with
  the active application revision. `orna source apply <file.orna>` checks and
  prepares one source file, records a typed application migration, and commits
  the candidate atomically. Diagnostics, an active-base race, an unsupported
  physical operation, or failed post-apply hash reproduction fails closed.
- The source compiler and accepted fixtures cover bounded server functions,
  typed scalar/reference/record values, identity-selected mutations, client
  expression bodies, local/session/user state declarations, resources and
  actions, JSON, bounded table presentation, and the accepted structural UI
  constructors. The [accepted demo inventory](examples/accepted-demos.toml)
  is the authoritative list of checked-in examples; it is not a replacement
  for the missing canonical spec.
- The CLI exposes function invocation, a bounded local function-backed REPL
  route, daemon/server administration, runtime description, source
  check/diff/apply, raw calls, USER state, inspection, and security
  administration. A local format-3 REPL with writable, identity-admitted source
  matching `HEAD` can commit a Blob capture and its source-row table insert as
  one durable runtime transaction without publishing a Git commit. Other
  effectful REPL submissions fail closed when matching format-3 transaction
  authority is unavailable. This does not establish engine
  execution for the canonical `ORNA-REPL-001..006` requirements. `--explain`,
  argument files, canonical output, JSON/table/CSV presenters, and bounded
  invocation tracing are implemented where the selected route supports them.
- On Linux, `orna mount DIR --cwd` mounts the current committed workspace
  through host FUSE and allows validated in-place row-document saves, including
  editors that open with `O_TRUNC`. Existing rows are validated and replaced
  before durable data changes; sibling temporary files renamed over `data.orna`
  remain supported. `orna unmount DIR` releases the kernel mount and its view
  record. `orna mount DIR --at SELECTOR` starts a read-only FUSE session pinned
  to one resolved commit; see the Unreleased section.
- Committed snapshot loading preserves nested module import candidates while
  keeping format-3 row documents out of loose-row source discovery.
- USER state is principal-scoped and typed. Local peer authentication supplies
  the principal; optimistic revision conflicts and invalid expected types fail
  closed. State values use the canonical ORV5 value representation.
- The system API exposes publication metadata as typed records. `sys.Storage`
  declares `pending_rows`, `pending_bytes`, `last_publication` and a
  `publication_policy` field of type `sys.Value`. Orna 1.1.0 defines no
  `sys.PublicationPolicy` type, and `sys.MaintenanceJob` has no row or byte
  counts.
- The Inspector exposes immutable invocation epochs, eight closed projections,
  redacted-by-default records, privilege checks, and sequence-addressable
  traces. Local SQLite inspection is narrower and stores only bounded,
  redacted invocation/inspection metadata; it does not expose durable resource
  payloads or arbitrary stored values.
- The installed `tty` runtime renders terminal documents and byte streams. The
  runtime/value boundary uses canonical ORV5 values and ORV6 for checked sets;
  output framing is validated before bytes are written. The Qt path is not a
  production-support claim for this inventory: its ABI/native checks require
  the absent canonical header and separately gated native dependencies.
- `orna-lsp`, the Tree-sitter grammar, the TextMate grammar, and editor
  integration packages provide the checked-in static tooling surface. LSP
  diagnostics, symbols, hover, references, completion, semantic tokens, and
  pull/push diagnostics are implemented. Static gates do not launch editor
  hosts and do not prove parity with the absent spec or host-session behavior.

### Storage and compatibility contracts

- Internal application migrations use a closed, typed, backend-neutral
  migration representation. The source is lowered by each storage adapter;
  users must not edit generated SQL or internal `_orna_kernel` tables. The
  application migration ledger retains expected and candidate source/catalogue
  revision pairs, canonical bytes, and digests.
- Revision recovery validates lineage, active pointers, ledger order, canonical
  digests, and post-apply reproduction. Historical source and standard
  snapshots are immutable inputs to this verification; hand-editing retained
  rows is unsupported.
- The CLI's `migrate copy` command prepares or recovers a journalled candidate
  in a separate directory from a local format-1/2 repository, leaving the
  source commit and workspace unchanged. It requires a decoded annotation-default
  count (`--annotation-defaults N`) and the `--no-persistent-consumers` flag.
  Despite its name, that flag confirms that persistent consumer checkpoints are
  carried, not reset. Each carried checkpoint must match a consumer identity
  correspondence recorded in the candidate before any row is imported. A source
  that holds live stream state (leases, claims, failures or receipts) is
  refused. This follows ORNA-UPGRADE-008. This preparation step does not
  convert legacy rows into a format-3 store or activate a writable format-3
  repository.
- Format-3 committed Blob rows require exact canonical MIME-1 media types and
  suffixes. Readers reject noncanonical stored annotations instead of
  normalizing them during metadata projection or SOV-3 conversion.
- A prepared Blob row binds its stored annotation to the annotation its pin was
  admitted with. The candidate metadata lookup rejects a row whose spelling is
  not canonical MIME-1, or that differs from the admitted pair, so a transfer
  record cannot report an annotation the pin never admitted.
- The CLI publication boundary validates format-3 rows against the pinned
  committed table binding and writes them to the committed format-3 store. It
  derives no loose-row paths. Loose-row paths are a format-1/2 layout only;
  Orna 1.1.0 forbids them for new-format writes (ORNA-UPGRADE-003). Invalid
  rows fail closed while preserving the pending range.
- `orna history <relation> --diff <from> <to>` decodes source-owned Object row
  fields against each endpoint's committed schema and compares stored Blob
  annotations without fetching payloads.
- Product selection retains standard-library snapshots through `orna.std/9`.
  The pre-1.0 V10 `std/cli.orna` source and V9-to-V10 upgrade constructor are
  retired; a stored V10 revision can only be verified when supplied directly
  and is never reconstructed or substituted. The pre-1.0 V11 SQL-shaped math
  addition is also retired; pure math comes from the pinned Orna 1.0 source.
- Local server sockets use the private Orna protocol handshake (versions 1–5,
  with bounded fallback behavior where an opaque codec registry is unavailable)
  and local peer authentication. These are private Orna contracts, not a
  public SQL or remote transport protocol.

### Explicitly deferred, unavailable, or not promised

The following boundaries are intentional and must not be inferred as shipped
from the implementation inventory:

- Full language/spec conformance, canonical examples, and ABI parity: the
  canonical bundle is absent, so no conformance or ABI result is claimed.
- The checked-in accepted corpus is intentionally bounded. Examples recorded
  as deferred include broader ALTER/drop/rename forms, procedural declarations,
  top-level INSERT, list/map/record literals and lambdas, broader SERVER
  expression calls/operators, additional CLIENT return forms, and
  value-type/user/role/grant/revoke declarations.
- Remote Orna TLS sessions, explicit Unix-socket routing beyond the bounded
  installed `invoke` route, reflective JSON-RPC/MCP gateways, a production
  CLIENT VM/sandbox, arbitrary toolkit or browser runtimes, and richer Studio
  workflows.
- SQLite physical/runtime parity beyond the bounded adapter capabilities
  described above. Unsupported SQLite physical shapes fail closed; the source
  language remains backend-neutral.
- Native Qt/ABI, editor-host, and clean-host distribution checks unless a named
  command has been run with retained evidence. `CARGO_NET_OFFLINE=true` only
  makes Cargo dependency resolution fail closed; it does not make a native
  build host- or network-free.
- A production package, repository publication, signing, SBOM/package
  inventory, support period, SLA, backup/DR policy, or recovery guarantee.
  CI retention artifacts and local build outputs are not production
  distribution authorities.
- The canonical live `sys.Session` relation, `sys.SessionRef`,
  `sys.rt.sessions`, and `sys.RuntimeView.sessions` projection. Commit
  `78301153` pins that schema in `sys_api_contract`, but it does not supply
  authoritative session-field provenance or runtime projection evidence.
- An in-place upgrade from a 0.x or development engine. The first-release
  decision defines empty accepted predecessor/forward-edge sets; no package
  predecessor migration is implemented. There is no `orna server upgrade`
  command in the current CLI.
- RPM packaging or cross-format upgrade support.

For migration steps and compatibility cautions, see the
[1.0.0 migration guide](docs/migration-1.0.0.md). For the commands, prerequisites,
and evidence vocabulary used by maintainers, see the
[maintainer runbook](docs/maintainer-runbook.md) and
[editor tooling guide](docs/editor-tooling.md).
