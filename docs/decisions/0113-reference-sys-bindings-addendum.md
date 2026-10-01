# External reference record: sys binding architecture addendum

**Status:** Documentation slice complete in the reference tree; this file records provenance and verification only.

**Beads issue:** `ornadb-btcjc`; GitHub issue [#5487](https://github.com/workingdir/ornadb/issues/5487).

## Recorded change

Added the standalone, explicitly non-normative reference addendum at:

`/home/pbox/dev/ornadb/reference/Orna-1.0.0/addenda/ADDENDUM-sys-bindings.md`

SHA-256: `c5e99a819782de1dd25e75ba7918834185f84bf959aee1b4b305a5494da94083`.

The reference directory is not a Git repository. The required `git -C /home/pbox/dev/ornadb/reference/Orna-1.0.0 status --short --branch` command returned “fatal: not a git repository (or any of the parent directories): .git” (exit code 128). No reference-tree VCS commit or push is possible. This repository change tracks the external documentation update; it does not contain or publish a copy of that file.

The addendum is a new file only. No chapter pointer was needed, so no hashed source or publication file was edited. It documents the distinction between the normative `api/sys.json` schema and its implementation provenance, notes that current merged code spells the operation annotation `#[ornasys]` while `#[sys_op]` is the requested architectural label, and keeps the WIT/Wasm extension boundary separate from built-in native `sys`.

## Verification record

### Beads link

Command: `env GITHUB_TOKEN="$(gh auth token)" BEADS_DIR=/home/pbox/dev/ornadb/.beads /home/pbox/.local/bin/bd github sync --push-only --issues ornadb-btcjc --verbose`

Captured output: `✓ Pushed 1 issues`. Exit code: `0`.

Command: `env BEADS_DIR=/home/pbox/dev/ornadb/.beads /home/pbox/.local/bin/bd show ornadb-btcjc --long --json`

Verified `external_ref=https://github.com/workingdir/ornadb/issues/5487`.

### Release checksums

Before and after adding the new file, the command `sha256sum release.json SHA256SUMS file-manifest.json source/manifest.json` returned the same values:

| Existing file | SHA-256 before | SHA-256 after |
|---|---|---|
| `release.json` | `f0f3ee5908d8ce2edef2bc14ca908cdea859f01b3dea27511206c2414a73d6e0` | `f0f3ee5908d8ce2edef2bc14ca908cdea859f01b3dea27511206c2414a73d6e0` |
| `SHA256SUMS` | `2152949a169e2e3494283db4aee7448ea6ecfef788789c998cda288e50b50315` | `2152949a169e2e3494283db4aee7448ea6ecfef788789c998cda288e50b50315` |
| `file-manifest.json` | `2c7844d1fc47681f47bb94ae811dd7c529cb224510fb562fe57759d14c05af3e` | `2c7844d1fc47681f47bb94ae811dd7c529cb224510fb562fe57759d14c05af3e` |
| `source/manifest.json` | `7b6fc36817360c24259bbd2eead054f3aa0af907ffb4d1d3a8d07b27232784b1` | `7b6fc36817360c24259bbd2eead054f3aa0af907ffb4d1d3a8d07b27232784b1` |

Command: `sha256sum --check --quiet SHA256SUMS`, run from the reference root. Captured output was empty; exit code: `0`.

The same manifest check confirmed `api/sys.json` at its existing SHA-256, `318b6d54f51d44e8117ffc91520dfd2dc2722cc023fdad51bd4dba601dd9abcb`. Its bytes and schema were not edited. The addendum is outside the existing release inventory; no checksum drift occurred among the existing release artifacts.

No Rust tests were run; this slice changes documentation only.
