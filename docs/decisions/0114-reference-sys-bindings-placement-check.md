# External reference placement verification: sys binding addendum

**Beads issue:** `ornadb-92exm`; GitHub issue [#5525](https://github.com/workingdir/ornadb/issues/5525).

## Result

The requested non-normative addendum already exists at `/home/pbox/dev/ornadb/reference/Orna-1.0.0/addenda/ADDENDUM-sys-bindings.md`. It was added in the prior authorized documentation slice and recorded by PR #5515. Its current SHA-256 is `c5e99a819782de1dd25e75ba7918834185f84bf959aee1b4b305a5494da94083`.

The current issue's `docs/decisions/0113-reference-sys-bindings-addendum.md` is a provenance/verification record, not the architecture text itself; the full architecture is already in the requested reference file. Rewriting that existing file would violate this issue's “new file only” and byte-identical requirements. Reference-tree files changed for this issue: **none**. The required file is present, clearly labeled non-normative, and covers the requested architecture.

## Before/after inventory

Before making any reference-tree change for this issue, a SHA-256 inventory was captured with:

`find . -type f -print0 | sort -z | xargs -0 sha256sum > /tmp/ornadb-92exm.before.sha256`

It contained 421 files. Its own SHA-256 was `7dfc842524aa97902ff535e2b36b4f6b79fefb816f1de6134df8fa94323b70cb`.

After verification, the same inventory command was captured to `/tmp/ornadb-92exm.after.sha256`. It also contained 421 files and had the same SHA-256, `7dfc842524aa97902ff535e2b36b4f6b79fefb816f1de6134df8fa94323b70cb`. `diff -u /tmp/ornadb-92exm.before.sha256 /tmp/ornadb-92exm.after.sha256` produced no output and exited 0. Thus every existing file in the reference tree, including all chapters and the addendum, remained byte-identical during this issue.

The required release files and system artifact also had identical before/after hashes:

| File | SHA-256 before and after |
|---|---|
| `release.json` | `f0f3ee5908d8ce2edef2bc14ca908cdea859f01b3dea27511206c2414a73d6e0` |
| `SHA256SUMS` | `2152949a169e2e3494283db4aee7448ea6ecfef788789c998cda288e50b50315` |
| `file-manifest.json` | `2c7844d1fc47681f47bb94ae811dd7c529cb224510fb562fe57759d14c05af3e` |
| `source/manifest.json` | `7b6fc36817360c24259bbd2eead054f3aa0af907ffb4d1d3a8d07b27232784b1` |
| `api/sys.json` | `318b6d54f51d44e8117ffc91520dfd2dc2722cc023fdad51bd4dba601dd9abcb` |
| `addenda/ADDENDUM-sys-bindings.md` | `c5e99a819782de1dd25e75ba7918834185f84bf959aee1b4b305a5494da94083` |

Command: `sha256sum --check --quiet SHA256SUMS` from the reference root. Output was empty; exit code: `0`.

## VCS and issue record

The reference directory has no Git repository. `git -C /home/pbox/dev/ornadb/reference/Orna-1.0.0 status --short --branch` returned “fatal: not a git repository (or any of the parent directories): .git” (exit code 128), so no reference-tree commit or push is possible.

The issue was synchronized with `env GITHUB_TOKEN="$(gh auth token)" BEADS_DIR=/home/pbox/dev/ornadb/.beads /home/pbox/.local/bin/bd github sync --push-only --issues ornadb-92exm --verbose`. Captured output: `✓ Pushed 1 issues`; exit code: `0`. `bd show ornadb-92exm --long --json` verified `external_ref=https://github.com/workingdir/ornadb/issues/5525`.

No Rust tests were run; this issue required only reference-tree verification.
