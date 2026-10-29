# Gate E evidence: media reader through the FUSE mount (2026-10-29)

Scope: ornadb-5il1.369 (Gate E media reader diagnostic record) and the evidence part of ornadb-5il1.245.
Gate E is the actual writable mount. This document covers the media-reader diagnostic for the E2 leg and the
environment facts that every Gate E item must carry. Gate item E2 selects the nightly `workspace-cli` record, not the per-PR diagnostic below.

## What the record proves

`crates/orna-cli-v1/tests/vfs_media_reader.rs` mounts a captured 64 MiB WAV Blob with
`orna mount --cwd` and reads it through the kernel FUSE path:

- `cwd_mount_64_mib_wav_blob_serves_media_reader_patterns_byte_exact`: stat reports the row's
  size; sequential 1 MiB reads cover the Blob and then EOF; reads at page, chunk and half-way
  boundaries and clipped at the end; seeks at and past EOF; seek-from-end tail reads; 256
  deterministic random ranges of up to 1 MiB; `posix_fadvise` Normal, Sequential, Random and
  WillNeed hints with bytes unchanged; the mount daemon's peak RSS stays below the Blob size (measured at 64 MiB only);
  and `mpv` plays the mounted WAV from 1000 s for 2 s when mpv is installed.
- `cwd_mount_concurrent_readers_get_exact_ranges_from_one_blob`: four threads issue 128
  positional reads each on one mounted 8 MiB Blob and every range matches the source.
- `cwd_mount_stat_and_readdir_do_not_fetch_corrupt_blob_payload` (ORNA-VFS-018): the captured
  loose Git object is overwritten with corrupt bytes. `stat` and `readdir` still succeed; the
  first `read` fails with EIO (errno 5). Metadata never fetches payload.

The mpv leg is not optional in CI: `ORNA_REQUIRE_MEDIA_PLAYER=1` turns a missing mpv into a
failure. Outside CI without mpv the test prints `SKIP` for that leg only; the mount and
pattern reads still run.

## Where the record is written

| Job | Recorder | Artifact |
| --- | --- | --- |
| `cli-tests.yml` `Record the Gate E media reader evidence` | `gate_evidence.py run --record gate-e-media-reader` | `gate-e-evidence-media-reader` |
| `nightly-gates.yml` cli shard | `gate_evidence.py run --record workspace-cli` (E2 select) | `evidence-workspace-cli` |

The `gate-e-media-reader` record is an uploaded diagnostic. No gate item in `gate-evidence-items.toml` selects it,
so it does not count toward any Gate E item. Item E2 selects the nightly `workspace-cli` record.

Each record holds the schema `ornadb-gate-evidence/1`, the commit, the exact command, host
facts (`uname`), the tool versions, the input environment variables, the exit status,
per-test outcomes and the SHA-256 of the log.

Tool versions recorded for the media reader (`--tool-version`):

- kernel: `uname -r`
- libfuse: `fusermount3 -V`, `fusermount -V`
- media player: `mpv --version`
- editors: `vim --version`, `emacs --version`

The E1 item reads the editor, kernel and libfuse versions from the `workspace-cli` record.
The editor packages are installed by the job's `apt-get install -y emacs-nox mpv vim` step.

## Environment required by the record

- `ORNA_REQUIRE_FUSE=1`: FUSE must be present; a missing `/dev/fuse` fails instead of skipping.
- `ORNA_REQUIRE_MEDIA_PLAYER=1`: mpv must be installed; `--require-command mpv` enforces it.
- `--require-command vim` and `--require-path /dev/fuse` fail the record before the test runs
  if either is missing.

## Limitations (honest)

- One player (mpv) is exercised, not a player matrix (ornadb-5il1.62).
- The 64 MiB fixture is generated; it is not a real recorded media file from a user library.
- Peak RSS is measured from `/proc/<pid>/status` `VmHWM` of the mount daemon only. It is not a
  whole-process budget for the test runner.
- Editor versions are the versions installed on the CI image; the record does not prove a
  particular editor build beyond `--version` output.
- The per-PR `cli-tests.yml` crate-wide step passes `--exclude-target test:vfs_media_reader`, so the suite runs once,
  in the `gate-e-media-reader` record. The nightly workspace-cli record runs it within the full crate suite.
- Peak RSS was measured at one Blob size (64 MiB). The check bounds the peak below the Blob size; it does not show
  the peak is independent of Blob length.

## Local run

Result of the focused proof on this worktree (`fix/ornadb-5il1.369-gate-e-evidence`, base
`origin/main` 6f2512726), run with `ORNA_REQUIRE_FUSE=1`:

```
cargo test --locked -p orna-cli-v1 --test vfs_media_reader
```

Results (three local runs on this worktree before the W282 fixes; `EXIT` is the cargo exit status):

- Run 1: `3 passed; 0 failed; 0 ignored`, finished in 67.70s, `EXIT=` (empty; the status line was not captured for that run). Mount daemon peak RSS after 64 MiB of reads: `51736 KiB` (from `VmHWM`).
- Run 2, after adding `allow(dead_code, unused_imports)` to the three `#[path]` support modules: `3 passed; 0 failed; 0 ignored`, finished in 67.71s, `EXIT=0`. The RSS line did not appear in this run's captured output, so no RSS figure is attributed to run 2.
- Run 3, with `ORNA_REQUIRE_MEDIA_PLAYER=1` (mpv at `/usr/bin/mpv`, CI configuration): `3 passed; 0 failed; 0 ignored`, finished in 67.54s, mount daemon peak RSS `51960 KiB`. The `EXIT=0` on this run is the status of `grep`, which was piped after cargo; the `test result: ok` line is the evidence for run 3.

The CI artifact for this head is `gate-e-evidence-media-reader` in the PR 9417 check run for the head commit. vim is not installed on this worktree's host, so the vim `--require-command` path was not exercised locally; CI installs vim with apt.

## Gate E items E2, E3, E6, E7, E8, E9 (ornadb-5il1.245)

These items are registered in `scripts/gate-evidence-items.toml` against the `workspace-cli` record. That record runs `gate_evidence.py test -p orna-cli-v1`, which covers every `tests/*.rs` target, so no CI step was added. Most selectors are existing mount tests; this change adds three new tests in existing files (E3 `rejected_sync_retains_recoverable_draft_listed_by_id`, and the two E8 selectors described below).

- E2: existing selector `cwd_mount_wav_blob_row_plays_through_seek_reads_and_exports_byte_for_byte` (`vfs_mount_integration`): a WAV Blob serves seek and range reads through the mount and exports byte-for-byte (profiles/vfs-1.md Blob fields). No selector was added for E2.
- E3: `rejected_sync_retains_recoverable_draft_listed_by_id` (`vfs_mount_sync_semantics`, new): an fsync refused with EINVAL leaves the row unchanged; the fields xattr is read while mounted and names the `sys.vfs.invalid_document` cause with a draft id and a non-empty diagnostic; a retain request then keeps the candidate as the record `<id>.draft` in recovery storage. The test asserts the record header names that id and cause, and that the record body after the header is the exact rejected candidate bytes (profiles/vfs-1.md:101-102, errno table, ORNA-VFS-009).
- E6: `blind_replacement_after_late_temp_overwrites_owner_change_while_orna_edit_refuses` (`vfs_mount_isolation`): demonstrates the limitation in profiles/vfs-1.md:118-119. A temp created after an owner change and renamed over the row replaces it; the same race through `orna edit` is refused with `error[E2000]: Edit is stale against the mounted baseline` and a help line that says to reapply the retained draft. The spec cause `sys.vfs.stale_edit` (Orna-1.1.0.md:8674) is not asserted. [INFERENCE] from reading fuse_mount.rs:3893: the explicit-edit refusal returns before the mount records a draft refusal, so the cause is not expected in `orna edit` output or in the mount's unresolved-draft fields; the test does not assert that absence. The test pins the limitation as current behaviour. It is not a protection.
- E7: `read_handle_opened_before_temp_rename_keeps_old_bytes` and `reader_opened_before_owner_write_keeps_old_bytes` (`vfs_mount_isolation`), and `cwd_mount_open_blob_handle_keeps_captured_generation_across_replacement` (`vfs_mount_integration`): handles opened before a rename-over or an owner write keep their captured bytes and size; new opens see the replacement.
- E8: `reserved_key_components_escape_first_byte` (`vfs_projection_features`, new): `.git`, `.orna` and `CON` project as `%2Egit`, `%2Eorna` and `%43ON` and each resolves its own row (profiles/vfs-1.md:31-33, 44-50). profiles/vfs-1.md:46 says "DOS device basenames" without listing them or stating case or extension rules, so the second selector `reserved_spelling_beyond_vfs_1_text_is_conservative` (new, spec-silent, conservative) covers `nul.tar`, `com1.txt`, `LPT9`, `CLOCK$` (`%43LOCK%24`), and lookalikes `conx`, `com10`, `console`. That selector pins the product's conservative choice, not a spec requirement.
  Key encoding: both documents describe the same editable row key components with different spellings. The VFS mount profile uses `%HH` with uppercase hex and `~empty` for the empty key (profiles/vfs-1.md:40-41) and is authoritative for the mount. `tests/path-vectors.json` (ORNA-PATH) uses `~hh` lowercase, so `.GIT` becomes `~2eGIT` and `PRN` becomes `~50RN`, and it encodes the empty component as `~ff` (path-vectors.json:63). The divergence is open and is not resolved here: bead ornadb-5il1.245.1 tracks the empty-key spelling and the `%HH` versus `~hh` conflict.
- E9: `unsynced_draft_is_invisible_to_second_open_and_after_child_exec_and_sigkill_remount` and `repeated_observer_reads_never_show_private_draft_and_read_handles_pin_generation` (`vfs_mount_isolation`): an unsynced draft is not visible to a second open, a new open, or a remount after SIGKILL, and repeated observer reads never show it. The mount-flag unit test (`editor_open_flags_bypass_kernel_cache_without_writeback_capability`, `bin:orna`) is not selected by this record.

Local results on `fix/ornadb-5il1.245-gate-e-items` (`CARGO_TARGET_DIR` on the shared lane, `ORNA_REQUIRE_FUSE=1`):

- `vfs_mount_sync_semantics rejected_sync_retains_recoverable_draft_listed_by_id --exact`: `test result: ok. 1 passed; 0 failed`.
- `vfs_projection_features reserved_` (both E8 selectors): `test result: ok. 2 passed; 0 failed`.
- `vfs_mount_isolation blind_replacement_after_late_temp`: `test result: ok. 1 passed; 0 failed`.
- `vfs_mount_isolation` (whole binary): 18 passed, 1 ignored (the pre-existing ignored test).
- `vfs_mount_integration` (the two selectors above): 2 passed.
- `vfs_mount_isolation` E9 test: failed once in three runs before a fix. The failure was `fresh_view` returning NotFound after the SIGKILL remount. `wait_for_mount` waits only for the root to answer, so the test now calls `await_path` on the projected row before reading. After the fix, ten consecutive single-test runs passed, and a later 20-iteration loop of the single test passed 20 of 20 (`cargo test -q -p orna-cli-v1 --test vfs_mount_isolation -- --exact <E9 name>`, exit 0 each time). The cause is [INFERENCE] a readiness race; the failure did not reproduce on demand.
- `python3 scripts/gate_evidence.py check`: exit 0.
- `python3 -m unittest scripts/test_gate_evidence.py`: 44 tests, OK.
- `vfs_*` suites that use the shared `await_path` (one run, `ORNA_REQUIRE_FUSE=1`): `vfs_media_reader` 3 passed; `vfs_mount_cases` 18 passed; `vfs_mount_integration` 58 passed; `vfs_mount_isolation` 18 passed, 1 ignored; `vfs_mount_sessions` 19 passed; `vfs_mount_sync_semantics` 32 passed; `vfs_mount_truncate_sparse` 9 passed; `vfs_projection_features` 14 passed, 1 ignored; `vfs_schema_change` 2 passed, 1 ignored. Every binary exited 0 with no failures.

No product defect was found, so no product change was made. Three new tests were added (E3 and the two E8 selectors). E3 failed once on a test-side ordering bug: it read the accepted view after the retain step, when the projected path was no longer readable; the read now happens before retention. The new tests were not shown red beforehand. The CI record for this head is not yet produced.

Limitations (honest):

- E2: the mpv leg runs only where mpv is installed (CI sets `ORNA_REQUIRE_MEDIA_PLAYER=1`); one player only (ornadb-5il1.62).
- E3: the retain request is the `user.orna.retain-drafts` xattr that orna issues before unmount. The test issues it while the mount is still up and does not unmount afterwards, so the unmount-time sequence itself is not exercised. The `orna edit --row` draft admission and report path is not covered by this item (ornadb-5il1.140).
- E6: the test demonstrates a limitation; it does not show a fix. The spec cause sys.vfs.stale_edit is not asserted by this test; the absence from the mount's draft fields is [INFERENCE] from fuse_mount.rs:3893 (see the E6 bullet above).
- E7: replacement shapes beyond rename-over and owner write have no selector here.
- E8: the spec-silent selector pins conservative choices beyond profiles/vfs-1.md:46; no selector covers composite keys, nested collections, large Str, or multi-file source edits, and the empty-key projection test is ignored (ornadb-5il1.63). The `%HH` versus `~hh` and `~empty` versus `~ff` differences are recorded, not reconciled (bead ornadb-5il1.245.1).
- E9: the mount-flag unit test is not selected by this record.
- Shared helper: `await_path` (`tests/support/editor_mount.rs`) is global to every `vfs_*` suite that calls it (`vfs_media_reader`, `vfs_mount_cases`, `vfs_mount_isolation`, `vfs_mount_sessions`, `vfs_projection_features`, `vfs_schema_change`). Its wait is used by those suites, not only by the E9 test; their results are in the verification section below.
