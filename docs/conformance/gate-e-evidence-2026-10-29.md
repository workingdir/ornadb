# Gate E evidence: media reader through the FUSE mount (2026-10-29)

Scope: ornadb-5il1.369 (Gate E evidence record) and the evidence part of ornadb-5il1.245.
Gate E is the actual writable mount. This record covers the media-reader leg of E2 and the
environment facts that every Gate E item must carry.

## What the record proves

`crates/orna-cli-v1/tests/vfs_media_reader.rs` mounts a captured 64 MiB WAV Blob with
`orna mount --cwd` and reads it through the kernel FUSE path:

- `cwd_mount_64_mib_wav_blob_serves_media_reader_patterns_byte_exact`: stat reports the row's
  size; sequential 1 MiB reads cover the Blob and then EOF; reads at page, chunk and half-way
  boundaries and clipped at the end; seeks at and past EOF; seek-from-end tail reads; 256
  deterministic random ranges of up to 1 MiB; `posix_fadvise` Normal, Sequential, Random and
  WillNeed hints with bytes unchanged; the mount daemon's peak RSS stays below the Blob size;
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
- The `cwd_mount_*` media tests are skipped in the crate-wide `cli-tests.yml` step and run only
  in their own record, so their outcome appears under `gate-e-media-reader`. The nightly
  workspace-cli record runs them within the full crate suite.

## Local run

Result of the focused proof on this worktree (`fix/ornadb-5il1.369-gate-e-evidence`, base
`origin/main` 6f2512726), run with `ORNA_REQUIRE_FUSE=1`:

```
cargo test --locked -p orna-cli-v1 --test vfs_media_reader
```

Results (both runs on this worktree, `EXIT` is the cargo exit status):

- Run 1: `3 passed; 0 failed; 0 ignored`, finished in 67.70s, `EXIT=` (empty; the status line was not captured for that run). Mount daemon peak RSS after 64 MiB of reads: `51736 KiB` (from `VmHWM`).
- Run 2, after adding `allow(dead_code, unused_imports)` to the three `#[path]` support modules: `3 passed; 0 failed; 0 ignored`, finished in 67.71s, `EXIT=0`. The RSS line did not appear in this run's captured output, so no RSS figure is attributed to run 2.
- Run 3, with `ORNA_REQUIRE_MEDIA_PLAYER=1` (mpv at `/usr/bin/mpv`, CI configuration): `3 passed; 0 failed; 0 ignored`, finished in 67.54s, mount daemon peak RSS `51960 KiB`. The `EXIT=0` on this run is the status of `grep`, which was piped after cargo; the `test result: ok` line is the evidence for run 3.

Run 3 is the proof of record, since it is the only run with the media player required. Run 2 is the same test without that variable. vim is not installed on this worktree's host, so the vim `--require-command` path was not exercised locally; CI installs vim with apt.
