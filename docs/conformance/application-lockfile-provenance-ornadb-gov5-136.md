# Application Cargo.lock diff provenance (ornadb-gov5.136)

## Finding

The uncommitted `Cargo.lock` deletion observed in checkout
`4e17471ba339d190eb8f26bdeb6b318ff060c640` is not a valid dependency fix. It
removes the `"sha2 0.10.9"` edge from `orna-application-v1`, whose library
imports `sha2::{Digest, Sha256}`. Against refreshed `origin/main` at the time
of inspection, that checkout predates 601 commits. Its author and intent could
not be established from the available Git history or Beads record.

## Provenance

At the inspected base, `crates/orna-application-v1/Cargo.toml` did not declare
`sha2`, even though `crates/orna-application-v1/src/lib.rs` imported it. The
lockfile still listed `sha2 0.10.9` as a package dependency. The sole local
lockfile hunk removed that entry, making the stale lock graph agree with the
stale manifest while leaving the source uncompilable.

Commit [`ec497ec2a53d98a1de9150481d65e2e62ad57fd2`](https://github.com/workingdir/ornadb/commit/ec497ec2a53d98a1de9150481d65e2e62ad57fd2),
`fix(application): declare sha2 dependency (#1389)`, added exactly
`sha2 = "0.10.9"` to the application manifest. Current `origin/main` contains
that direct dependency and the corresponding `"sha2 0.10.9"` lockfile edge.
The application dependency declaration is the fix; deleting the lockfile edge
would undo the buildable graph.

## Reproduction

On the clean historical revision, `cargo check --locked -p
orna-application-v1` exited 101 because the manifest and lockfile disagree.
After allowing Cargo to reconcile that historical lockfile offline,
`cargo check --offline -p orna-application-v1` exited 101 with `E0432`:

```text
error[E0432]: unresolved import `sha2`
  --> crates/orna-application-v1/src/lib.rs:24:5
24 | use sha2::{Digest, Sha256};
   |     ^^^^ use of unresolved module or unlinked crate `sha2`
EXIT_CODE=101
```

At current `origin/main` (`a3c207a3354b935ebd257b32d85038ff214a1e4a`),
`cargo check --locked -p orna-application-v1` passed:

```text
Checking orna-application-v1 v1.0.0
Finished `dev` profile [unoptimized] target(s) in 1m 16s
EXIT_CODE=0
```

The shared historical checkout was left unchanged. No Cargo.lock regeneration
or source change is needed on current main; preserve the `sha2` manifest and
lockfile entries established by #1389.

## Child audit: exact diff and frozen reference boundary

The scoped working tree `/home/pbox/dev/ornadb/fresh` is at
`4e17471ba339d190eb8f26bdeb6b318ff060c640`. Its read-only status/diff output
shows `Cargo.lock` modified and no change to
`crates/orna-application-v1/Cargo.toml`. The complete relevant lock hunk is:

```diff
@@ -1239,7 +1239,6 @@ dependencies = [
  "orna-runtime-v1",
  "orna-semantic-v1",
  "orna-syntax-v1",
- "sha2 0.10.9",
 ]
```

This is an uncommitted worktree deletion. It has no commit object, author, or
commit message, and the persisted issue/session notes do not identify who made
it or why. Attribution of that deletion remains unresolved.

The removed lock edge itself has a separate, traceable history:

* `25651bac66fbcc72e9ae0ee6c6c6262399f4dc8e`,
  `cli: add orna-client dependency edge for ORNA-REPL-006 watch (gov5.99.1)`,
  added both `"sha2 0.10.9"` to the `orna-application-v1` lock entry and
  `"orna-client"` to the `orna-cli-v1` lock entry. `git blame` on the current
  application lock entry attributes the `sha2` line to this commit. At that
  commit, the application manifest had no `sha2` declaration while its source
  already imported and called `sha2::{Digest, Sha256}`. The lock edge therefore
  predates the explicit manifest declaration.
* `ec497ec2a53d98a1de9150481d65e2e62ad57fd2`,
  `fix(application): declare sha2 dependency (#1389)`, added exactly
  `sha2 = "0.10.9"` to `crates/orna-application-v1/Cargo.toml`. It is an
  ancestor of current `origin/main` at `b44b60851e790bd1e4587850112f8329d1d3a910`.
  Current main has the manifest declaration, source import, and lock edge.

The frozen reference sets behavior-level boundaries: `source/23-storage.md`
requires a SHA-256 content checksum for the described manifest, and
`source/29-formats.md` defines domain-separated SHA256 digest bytes. Neither
section prescribes Rust crates, Cargo manifests, or lockfile entries. The
dependency question is implementation/build provenance, not a normative
Cargo-package requirement. The tracked deletion on the old tree removes a
needed build edge for code that uses `sha2`; its original actor/intent cannot
be inferred from the diff. Current main already carries the manifest fix and
matching lock edge, so this audit does not call for a Cargo change.

No tests or Cargo validation were run for this read-only provenance audit.
