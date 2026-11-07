# Gate E disclosures: FUSE callbacks, close errors and blind replacement (2026-11-07)

Scope: ornadb-5il1.369. This document records the disclosures that ORNA-VFS-007 requires, and the
blind-replacement limitation that `implementation/ACCEPTANCE.md` (Gate E) asks to demonstrate rather than
claim impossible. It is a disclosure, not an acceptance claim: `reference/Orna-1.1.0/tests/requirement-evidence.json`
records ORNA-VFS-007 as `not-executed` with `full_implementation_coverage_claimed: false`.

The media-reader record and tool-version capture are in `gate-e-evidence-2026-10-29.md`.

## Requirement

ORNA-VFS-007 (`reference/Orna-1.1.0/source/41-vfs.md:60`): FUSE `release` MUST NOT be the only
rejection or durability point; callback behaviour and close-error limitations MUST be disclosed.

## Callback behaviour

- `release` never commits. Covered by the `orna-runtime` unit test
  `accepted_fsync_advances_only_future_opens_and_release_never_commits` in `crates/orna-runtime/src/vfs.rs`.
- Acceptance happens at `fsync`/`fdatasync` and at the `FLUSH` that a `close` sends. An accepted close flush
  succeeds, and the candidate is visible to a new open. Covered by `close_flush_accepts_dirty_draft_once`
  (`crates/orna-cli/tests/vfs_mount_sync_semantics.rs`), which also checks that an accepted close leaves no
  draft guard and retains no draft.
- The close flush compares against the row the handle captured at open (ORNA-VFS-011). A first writer that
  closes after a second writer's accepted commit is refused as stale and its candidate is retained
  (CHANGELOG Unreleased, ornadb-5il1.470.1). Covered by `stale_close_flush_keeps_peer_row_and_retains_candidate`.
- Flush ownership is by the pid that opened the handle. A `FLUSH` from any other process is acknowledged without
  accepting the draft; the opener's own close commits it (CHANGELOG Unreleased, ornadb-5il1.64). This is this
  build's policy. It is not the EDIT-1 step 5 text, which CHANGELOG notes. Not covered by this policy: a `FLUSH`
  from a duplicate of the writer's own descriptor in the same process, and `fsync` from another process; both
  still accept.

## Close-error limitation

- A refused close flush is reported to the application only through the `close(2)` result. The disclosure test
  `ignored_close_error_leaves_row_unchanged_and_draft_retained` (`vfs_mount_sync_semantics.rs`) models an
  application that ignores that result: the durable row stays unchanged, the refused candidate is retained in
  recovery storage, and it holds the draft guard. `orna mount status` lists the retained draft with its cause code
  (CHANGELOG Unreleased).
- Known limit (ornadb-5il1.64): if the descriptor is shared with another writer through `fork(2)` without
  `exec(2)`, or through `SCM_RIGHTS`, and the opener closes or exits first, the opener's `FLUSH` is the owner flush
  and commits whatever is dirty at that moment. Writes the other process makes afterwards are acknowledged on close
  without acceptance and stay unresolved (CHANGELOG Unreleased).

## Blind replacement (Gate E)

- An editor reads the row, another writer commits an owner change, and the editor then creates a temporary file
  and renames it over the row. The rename is admitted, and the owner change is silently replaced. This is
  demonstrated, not refused: `blind_replacement_after_late_temp_overwrites_owner_change_while_orna_edit_refuses`
  (`crates/orna-cli/tests/vfs_mount_isolation.rs`). The same race through `orna edit`, which holds a strong read
  baseline, is refused with E2000.
- Open reads keep old content after replacement: `read_handle_opened_before_temp_rename_keeps_old_bytes` and
  `reader_opened_before_owner_write_keeps_old_bytes` (`vfs_mount_isolation.rs`).

## Not covered by this document

- ornadb-5il1.64 (blind replacement and the owner-pid limit) remains open. This document discloses it and does not
  close it.
