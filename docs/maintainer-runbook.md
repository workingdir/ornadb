# OrnaDB maintainer and operator runbook

This runbook describes the current implementation in the v1 crates. The
target is Orna 1.1.0 (publication `final-2026-10-05`, repository writer format
3). It is maintained operational guidance, not a replacement for the canonical
publication under `reference/Orna-1.1.0/`. Historical design records remain in
`docs/decisions/` and are not operational inputs.

## Before you start

Work from the repository root. The command is `orna`. In a checkout, Cargo
builds it as the `orna` binary. The Linux package installs it as
`/usr/bin/orna`. Run it from a checked-out repository with the locked
dependency graph:

```text
cargo run --locked -p orna-cli-v1 -- --help
```

The examples below write `orna`. In a checkout, use
`cargo run --locked -p orna-cli-v1 --` in its place.

The workspace declares Rust 1.95. Git and Cargo are required for project
initialisation and source loading. Use `--offline` only after the locked
registry and Git dependencies are already cached; an offline command that
cannot resolve its cache is unavailable, not a successful check.

Keep the working tree and the runtime state it owns together. Do not copy or
hand-edit private runtime files to move state between repositories. The v1
repository layer resolves the per-worktree runtime location.

## CLI entry points

The command syntax is:

```text
orna [--db ENDPOINT] [--color auto|always|never] [--debug] COMMAND [ARGUMENTS]
```

Global options must precede the command. `--db PATH` selects a local Git
worktree. `init` takes its directory as a positional argument and rejects
`--db`. `--format human|short|json` is also accepted before `status` only.
With no command, `orna` starts the REPL.

This table lists every command that the parser in
`crates/orna-cli-v1/src/cli_args.rs` accepts. `orna --help` shows the
options and exit statuses of each command. The table does not claim that a
command meets every Orna 1.1.0 requirement.

| Command | Behaviour |
| --- | --- |
| `repl [EXPRESSION]` | Start an ephemeral interactive session, or evaluate one expression and exit. |
| `run [QUALIFIED_FUNCTION]` | Run a project function. With no target, run `main.main`. |
| `check` | Load reachable source modules from the local Git worktree and check them. |
| `invoke TARGET` | Execute a reachable zero-argument pure function. |
| `explain CODE` | Print guidance for a documented Orna diagnostic code. |
| `init [DIRECTORY]` | Initialise a local Git-backed Orna repository. The current directory is the default. |
| `add --row TABLE KEY` | Stage one logical row into the index. |
| `mv OLD NEW` | Rename a table in its module namespace and keep its IDs. |
| `status [--porcelain\|--short\|--format human\|short\|json]` | Show the worktree status. |
| `fetch [REMOTE] [BRANCH]` | Fetch a branch. The defaults are `origin` and `main`, not the current branch. |
| `push [REMOTE] [BRANCH]` | Push the branch with its continuity refs. The default remote is `origin`. |
| `serve [--port PORT]` | Serve the project on loopback. The default port is 8080. |
| `mount DIR --cwd` | Mount the committed workspace as an editable view. Row saves are validated. Linux only. |
| `mount DIR --at SELECTOR` | Mount one resolved snapshot as a read-only view. Linux only. |
| `mount status [--json]` | Report attached views. |
| `edit [--row] PATH` | Edit one projected row or Blob through transactional admission. |
| `unmount DIR` | Detach the kernel mount, including an orphaned Orna mount, and release its view record. |
| `diff [GIT_DIFF_ARGS...]` | Run Git `diff` for the worktree. |
| `log [GIT_LOG_ARGS...]` | Run Git `log` for the worktree. |
| `annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]` | Write an annotated Blob file. |
| `import BUNDLE --dry-run [OPTIONS]` | Report what an import bundle contains. |
| `export DEST --at SELECTOR [OPTIONS]` | Write, check or restore an offline archive. |
| `migrate copy DEST --annotation-defaults N --no-persistent-consumers` | Prepare a format-3 candidate from a format-1/2 worktree. The flag confirms that consumer checkpoints are carried, not reset. |
| `publish [--message MESSAGE]` | Commit the durable runtime tail as one publication commit. |
| `commit -m MESSAGE` | Review and publish exactly the staged candidate. |
| `history RELATION KEY [OPTIONS]`, `history RELATION --diff FROM TO` | List row history, or compare two snapshots without payloads. |
| `query RELATION [OPTIONS]` | List Blob metadata of committed rows without payloads. |
| `semantic-legend [OPTIONS]` | Print the editor semantic-token legend. |
| `--help`, `-h`, `help`, `--version`, `-V` | Print command help or the binary version. |

Examples:

```text
orna init ./example-project
orna --db ./example-project check
orna --db ./example-project invoke main
orna --db ./example-project run main.main
orna repl "1 + 2"
orna explain ORNA-S010-IMPORT
```

### Initialising a project

`init` creates missing repository metadata and an empty root module without
staging files or creating a commit. Existing root source is preserved and
repeated initialisation retains the database identity. Partial, malformed, or
unsupported metadata is reported rather than overwritten. Git's configured
defaults determine the initial branch. New database creation is currently
supported on Linux.

After adding source files, use `status` to inspect the worktree and `check` to
validate the reachable source. `check` is local and read-only with respect to
project source; it does not substitute a host catalogue or silently import
uncaptured modules. A successful check prints `project valid`.

### Pure evaluation and the REPL

`invoke TARGET` resolves a reachable zero-argument pure function. `repl` is an
ephemeral session: bindings, pure function declarations, imports, and `$_`
(the last successful result) remain available until EOF or `:quit`. A failed
submission leaves the retained declarations and last successful result
unchanged. Preview evaluation is bounded and cannot perform external effects.

The REPL can select an admitted snapshot with `:at CWD`, `:at HEAD`, or
`:at REF`. `HEAD` and named references are read-only committed snapshots; a
failed selection retains the previous session. Remote endpoints are not a
fallback for a local project: use a local Git worktree and an explicit `--db
PATH` when selecting a project.

The REPL help text lists `:help [name]` (`REPL_HELP` in `crates/orna-cli-v1/src/repl.rs`). The
`:watch expression` command needs a host-owned live watch source. The `orna`
binary does not wire one, so `:watch` reports `ORNA-REPL-COMMAND` there;
it is exercised only by the test harness in `crates/orna-cli-v1/tests/watch_command.rs`.

### Diagnostics and exit behaviour

Diagnostics are written to standard error. `explain CODE` accepts a documented
code such as `ORNA-S010-IMPORT`; an unknown code is a usage error. Unsupported
declarations or operations return an explicit diagnostic rather than silently
executing an unsupported operation. Keep the complete diagnostic code and help
text when recording an incident; do not replace it with a generic
success/failure label.

## Embedded libsql state

The v1 runtime uses the `libsql` crate's local database builder for private
state; it does not require a separate database service. `orna-runtime-v1`
opens the `state.db` path supplied by the repository's per-worktree runtime
paths, creates the database locally, and applies the runtime schema before
admitting work. The schema enables WAL journaling and foreign-key enforcement.

This state is below the Git boundary. It carries runtime metadata, writer-lease
and recovery state, pending mutations, checkpoints, failures, catalogue
snapshots, and other bounded observations that must share a local transaction.
The runtime owns its schema and validates recovery before returning an opened
state. Operators must not edit the database directly or delete its files to
resolve a failed operation; preserve the repository and runtime evidence and
rerun the affected v1 command after diagnosing the reported error.

A worktree has one writable runtime owner at a time. Runtime leases and epochs
fence stale owners and recovery attempts. A command that cannot acquire or
resume the local state must fail closed; it must not silently bind a request to
another worktree or to a newer snapshot.

The local state database is not a publication format. Format-3 rows live in the
committed native store, which publication writes. Format-1/2 loose row files and
compact data are read-only legacy layouts. The repository and storage readers
verify and read them, no production path writes them, and no CLI verb reads
them yet.
Publication and durable runtime writes on a format-1/2 `HEAD` are refused, and
no in-place format-3 upgrade ships yet (ornadb-5il1.184). Callers use the logical
table interface and do not branch on physical placement. Compact files and
manifests must be treated as immutable published artifacts and verified before
they become authoritative.

## Linux FUSE editor drafts

When an editor writes a replacement file and calls `fsync`, the writable
Linux FUSE mount stores that private scratch candidate in the worktree runtime
directory before acknowledging the call. The next writable mount restores each
synced scratch under its row directory after the mount process exits.
The scratch remains a draft: the VFS accepts it and creates the row mutation
only after it validates a temporary-file rename. Keep the runtime directory
intact; don't edit or delete its scratch records manually.

## Focused maintenance checks

Run the smallest check that covers the change. These are package-scoped checks,
not a claim that the whole product is validated:

```text
cargo test --locked -p orna-cli-v1 --test init
cargo test --locked -p orna-cli-v1 --test project_runtime
ORNA_REQUIRE_FUSE=1 cargo test --locked -p orna-cli-v1 --test vfs_editor_compat
cargo test --locked -p orna-runtime-v1 --lib
```

For a command smoke check, use a temporary local project and exercise
`init`, `check`, `status --porcelain`, and `repl "1 + 2"`. Keep the command
output and exit status with the evidence. A test or command that could not run
because a tool, dependency, or reference bundle is missing is unavailable; it
is not a pass.

Before committing documentation, run:

```text
git diff --check -- docs/maintainer-runbook.md
```

Record the exact command, working directory, revision, exit status, and any
unrun checks. Do not claim language, runtime, storage, or interoperability
completion from a CLI smoke alone.

## Standard library submodule bump

The Orna standard library source is the `stdlib/std` git submodule, which
tracks `workingdir/ornadb-std`. The superproject gitlink is the only source of
truth for which commit ornadb builds against. The Rust mirror in
`crates/orna-standard-sources/src/lib.rs` embeds those bytes with `include_str!`
and must match the pinned blobs byte for byte. Snapshot identities and accepted
digests are governed by `docs/decisions/0055-standard-executable-source-units.md`
and the later snapshot ADRs (0058, 0059); they are not regenerated in place.

Bump procedure:

1. Make the stdlib change in `workingdir/ornadb-std` on a branch, open a PR,
   and merge it to `main` only after its checks are green and it is reviewed.
   The pinned commit MUST be reachable from `ornadb-std` `main`. Never
   force-push or rewrite that history.
2. In the ornadb worktree, fetch the new commit into the submodule with
   `git -C stdlib/std fetch --depth 1 origin <sha>`, check it out with
   `git -C stdlib/std checkout --detach <sha>`, then stage the gitlink with
   `git add stdlib/std`. Do not change git configuration.
3. Mirror the changed files in `crates/orna-standard-sources/src/lib.rs`. A
   changed file keeps its `include_str!` path, so only a new file needs edits:
   a source const, a `REFERENCE_STANDARD_*_PATH_V1` const, an entry in
   `reference_standard_sources_v1`, and the `[(String, String); N]` length bumped
   everywhere that type appears. Every tracked `.orna` file must have an
   `include_str!` entry, or an entry in `UNMIRRORED_SOURCES` in
   `scripts/check_standard_library_pin.py` with a reason. The checker fails when
   a path const and its source const do not pair.
4. Do not treat the `orna.std/N` ids or the `ACCEPTED_*_SOURCE_REVISION_DIGEST`
   constants as part of a stdlib change. They cover only the retained SQL-era
   database snapshots (ADR 0055/0059) and are unrelated to the mirrored
   `stdlib/std` modules, whose digests are derived from their bytes at load time.
   The Rust evaluator's `STANDARD_BINDING_MODULES` registry is keyed by function
   name only, so removing a Rust shadow can break historical pins whose stub
   bodies self-recurse (`bit_or = __bit_or = std.bits.bit_or`). Gate any shadow
   removal on the module's pinned digest or stub shape, following
   `captured_timezone_snapshot_matches`. Keep the shadow for the old stub digests
   so ORNA-LIB-001 holds for historical pins.
5. Run the drift check and the pin-checker tests from the worktree root:

```text
python3 -m unittest scripts.test_standard_library_pin
python3 scripts/check_standard_library_pin.py --check
```

The check fails when the gitlink is not on `ornadb-std` `main`, the submodule
is dirty or at a different commit, or a mirrored module differs from the pinned
blob. CI runs the same two commands in the `fixture-and-editor-artifacts` job of
`.github/workflows/source-audits.yml`, after a recursive submodule checkout.

Then run the package-scoped Rust checks that cover the snapshot you changed
(for example `cargo test --locked -p orna-evaluator-v1 --test standard_profile_v1`), with the cargo limits from
the repository rules. A check that could not run is unavailable, not a pass.

### Moving a Rust shadow into the standard library

Language features belong in `stdlib/std`, not in Rust. To move one module's
shadowed functions into Orna bodies (use a module whose functions Orna can
express; the bits primitives cannot move because Orna has no bitwise operator):

1. In `workingdir/ornadb-std`, branch and replace the stub bodies (for example
   `pub fn encode = __encode; fn __encode = encode`) with real Orna bodies.
   Keep public signatures unchanged. Merge through a normal reviewed PR; never
   force-push.
2. In the ornadb worktree, follow the bump procedure steps 2 and 3 for the new
   commit. A changed file needs no mirror edit; a new file needs the consts and
   array entry described in step 3.
3. Gate the Rust shadow before removing it. Key the decision on the module's
   pinned digest or stub shape, following `captured_timezone_snapshot_matches`.
   Then drop the op from `STANDARD_BINDING_MODULES` in
   `crates/orna-evaluator-v1/src/lib.rs`, its dispatch arm, and its arity-name
   table entry, for pins that carry real bodies. Keep the shadow for the old
   stub digests so historical pins behave identically under ORNA-LIB-001.
4. Update the tests that pin the old shape: the `stub_candidates == 30` and
   `registered >= 4` asserts in `registry_covers_pinned_intrinsic_and_sys_host_stubs`,
   the module's `standard_library_*` evaluator tests, and the declaration and
   index asserts in `crates/orna-evaluator-v1/tests/standard_profile_v1.rs`. Check `scripts/fixture-manifest.sha256` only
   if fixtures change.
5. Add a golden test for each moved function that runs the new bytes with no
   shadow, and a replay test (as in `standard_snapshot_replay.rs`) showing that the
   old stub commit still behaves identically.
6. Run `python3 scripts/check_standard_library_pin.py --check`, then
   `cargo test --locked -p orna-evaluator-v1` (this includes `standard_profile_v1`), under the
   repository cargo limits, then the fixture-manifest check.

## 1.1.0 requirement evidence report

From the repository root, generate the machine-readable report against the
frozen reference bundle beside this checkout:

```text
cargo run --locked -p orna-traceability-v1 -- ../reference/Orna-1.1.0 > /tmp/orna-traceability.json
```

The report pins the specification version and publication digests. It lists
each numbered requirement with its chapter, aggregate status, and evidence
boundaries; it also lists normative payloads, fixture classes, behavioural
scenarios, and any bounded production-evidence records. The command reads the
reference bundle. It does not discover implementation or test evidence from
the current checkout or from Beads. Its baseline can therefore mark every
requirement `justified-gap`; that means the report has no passing execution
witness for the obligation, not that a test-plan entry or source file is a
passing implementation test. `implemented`, `optional`, or `out-of-scope` are
not report statuses.

Treat the report as traceability input, not a conformance claim. The reference
bundle's `tests/requirement-evidence.json` associates requirements with evidence
obligations; it does not say those tests ran. Keep any implementation and test
references tied to captured commands and actual results, and retain partial
status when evidence covers only a bounded subset. A generated index,
fixture count, parsed example, or successful report command alone cannot
establish execution or conformance. These limits follow ORNA-TEST-004
(source/32-conformance.md:23), ORNA-TEST-010 (line 31), ORNA-TEST-011
(line 33), and ORNA-EVIDENCE-001 (line 69), and ORNA-CONF-001 through
ORNA-CONF-003 (Orna-1.1.0.md:65–69) in the frozen reference.

Before making a conformance claim, record the claimed class and implementation
version, the exact reference publication digest, supported optional profiles,
mandatory facilities, and the implementation tests actually executed. Claim
only classes for which every applicable fixture and behavioural test has a
passing result; parser success is not runtime evidence. The baseline report
does not supply those claim details.

## Issue ledger and historical material

`.beads/` is the tracked issue ledger. Use the repository's Beads workflow for
issue state and preserve its history; do not replace it with a generated status
file. Current operational claims belong in this runbook and in executable v1
checks. Historical design decisions are context only and must not be used as
instructions for current commands.

When changing this runbook, keep the edit limited to current v1 behaviour. Do
not reintroduce superseded command names, backend-specific service setup, or
package paths that are absent from the v1 CLI and runtime path.
