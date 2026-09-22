# OrnaDB maintainer and operator runbook

This runbook describes the current Orna 1.0 implementation in the v1 crates. It
is maintained operational guidance, not a replacement for the canonical
publication under `reference/Orna-1.0.0/`. Historical design records remain in
`docs/decisions/` and are not operational inputs.

## Before you start

Work from the repository root. The v1 command is the `orna-cli-v1` Cargo
binary; run it from a checked-out repository with the locked dependency graph:

```text
cargo run --locked -p orna-cli-v1 -- --help
```

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
cargo run --locked -p orna-cli-v1 -- [--db PATH] COMMAND [ARGUMENTS]
```

`--db PATH` must precede the command and selects a local project directory. It
is accepted by project-loading commands; `init` takes its directory as a
positional argument and rejects `--db`.

| Command | Behaviour |
| --- | --- |
| `init [DIRECTORY]` | Initialise a local Git-backed Orna repository. The current directory is the default. |
| `status` | Show the local Git worktree status. |
| `status --porcelain` | Emit the machine-readable status form. |
| `status --short` | Emit the short status form. |
| `check` | Load reachable source modules from the local Git worktree and perform the v1 checks. |
| `invoke TARGET` | Execute a reachable zero-argument pure function. |
| `run` | Run the default project entry point (`main.main`). |
| `run TARGET` | Run a named project entry point. |
| `run seed` | Run the reference seed workflow when the loaded project provides it. |
| `run exercise` | Run the reference exercise workflow when the loaded project provides it. |
| `run sensors.ingest` | Run the finite sensor-ingestion workflow when the loaded project provides it. |
| `run library.lend BOOK BORROWER` | Run the bounded lending workflow with the supplied identifiers. |
| `repl [EXPRESSION]` | Evaluate a pure expression in an ephemeral interactive session, or evaluate one expression and exit. |
| `explain CODE` | Print guidance for a documented Orna diagnostic code. |
| `--help`, `help`, `--version`, `-V` | Print command help or the binary version. |

Examples:

```text
cargo run --locked -p orna-cli-v1 -- init ./example-project
cargo run --locked -p orna-cli-v1 -- --db ./example-project check
cargo run --locked -p orna-cli-v1 -- --db ./example-project invoke main
cargo run --locked -p orna-cli-v1 -- --db ./example-project run main.main
cargo run --locked -p orna-cli-v1 -- repl "1 + 2"
cargo run --locked -p orna-cli-v1 -- explain ORNA-S010-IMPORT
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

The local state database is not a publication format. Git repository history,
loose row files, and compact data are managed by their respective v1 layers;
callers use the logical table interface and do not branch on physical
placement. Compact files and manifests must be treated as immutable published
artifacts and verified before they become authoritative.

## Focused maintenance checks

Run the smallest check that covers the change. These are package-scoped checks,
not a claim that the whole product is validated:

```text
cargo test --locked -p orna-cli-v1 --test init
cargo test --locked -p orna-cli-v1 --test project_runtime
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

## Issue ledger and historical material

`.beads/` is the tracked issue ledger. Use the repository's Beads workflow for
issue state and preserve its history; do not replace it with a generated status
file. Current operational claims belong in this runbook and in executable v1
checks. Historical design decisions are context only and must not be used as
instructions for current commands.

When changing this runbook, keep the edit limited to current v1 behaviour. Do
not reintroduce superseded command names, backend-specific service setup, or
package paths that are absent from the v1 CLI and runtime path.
