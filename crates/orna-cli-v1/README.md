# orna-cli-v1

A developing CLI for the current Orna 1.0 source language.

## Command surface

This binary currently accepts `init`, `status`, `fetch`, `push`, `serve`,
`mount`, `unmount`, `diff`, `log`, `annotate`, `import`, `export`, `migrate copy`,
`publish`, `semantic-legend`, `history`, `query`, `check`, `invoke`, `explain`,
`repl`, and `run`, plus help and version options. The available command groups and
option forms are listed by `orna --help`; that list describes this bounded
implementation, not the complete Orna 1.0 command set.

The frozen CLI reference also names `serve`, `fmt`, `verify`, and `prune`, as
well as Git-compatible operations that this binary does not yet implement.
Their presence in the canonical list is not a claim that this binary supports
them. The package remains explicit about the boundary rather than accepting
placeholder commands without their command-specific behavior.

The reference requires no separate `stream`, `checkpoint`, `failure`,
`grants`, or extension-permission command hierarchies. Under `ORNA-CLI-002`,
runtime, checkpoint, failure, and uncommon recovery details belong in typed
`sys` values available to REPL code and optional `std.devtools` pages. Storage
preference and rewrite operations likewise remain typed administration rather
than a built-in command family (`ORNA-STORAGE-010`).

`ORNA-CLI-001` covers Git-compatible command naming, flags, and observable
semantics; `ORNA-DIAG-002` covers extended diagnostic documentation through
`explain`. See the frozen reference's `source/18-cli.md`, `source/16-administration.md`,
`source/17-repl.md`, `source/19-repository.md`, `source/23-storage.md`, and
`source/34-system-reference.md` for those boundaries. This package does not
claim the unimplemented command surface as complete.

`init [DIRECTORY]` initializes a local Git-backed database, using the current
directory when no target is supplied. It creates missing repository metadata
and an empty root module without staging files or creating a commit. Existing
root source is preserved, and repeated initialization retains the database
identity. Partial, malformed, or unsupported metadata is reported rather than
overwritten. Git's configured defaults determine the initial branch.
Use a positional directory for initialization, not `--db`.
Creating a new database is currently supported on Linux.

`migrate copy` prepares or recovers a journalled migration candidate in a
separate directory from a local format-1 or format-2 worktree. For example:

```text
orna --db ./legacy migrate copy ./migrated --annotation-defaults 0 --no-persistent-consumers
```

`--annotation-defaults N` must be the count derived from decoded legacy
annotation metadata. `--no-persistent-consumers` is an explicit declaration
that no persistent consumer continuity needs to be carried. The source
repository, including its commit and dirty workspace, remains unchanged. This
command prepares a journalled candidate only; it does not convert legacy rows
into a format-3 store or activate a writable format-3 repository.

`check` loads and checks reachable source modules from a local Git worktree.
`invoke TARGET` executes a reachable zero-argument pure function. The optional
`--db PATH` argument selects a local project explicitly.

`repl` starts an ephemeral interactive session; `repl EXPRESSION` submits one
expression. Results include a structural representation and type. Session
bindings, pure function declarations, and imports remain available until EOF
or `:quit`. `$_` retains the last successful expression result, including after
a failed submission. Starting another session discards the previous state.
Use `--db PATH repl` to import from an explicitly selected, checked pure project.

For example:

```text
let n: Int = 21;
fn twice(value: Int): Int = value + value;
twice(n)
$_
:quit
```

## Workspace mounts

On Linux, `orna mount DIR --cwd` mounts the current committed workspace at
`DIR` through host FUSE. It requires a committed HEAD and allows edits to
projected `data.orna` row documents. The mount validates a document before it
changes the durable row, and supports editors that write a sibling temporary
file, sync it, then rename it over `data.orna`. The command stays in the
foreground until `orna unmount DIR` releases the kernel mount and view record.

`orna edit --row /path/to/mount/Table/key` opens the row’s `data.orna` in
`$EDITOR`. Its absolute row-directory path must be inside the active Orna FUSE
mount; the edit is validated before the durable row changes.

`orna mount DIR --at SELECTOR` attaches a read-only snapshot record for one
resolved selector without starting a host FUSE mount. `orna mount status`
reports attached views in human or `--json` form. Host FUSE mounts are supported
on Linux only.

## Boundary

Execution currently uses the bounded pure evaluator. Unsupported declarations
or submitted operations report errors; they do not run effects. Preview uses
a separate evaluation entrypoint that cannot change session state or perform
external effects. Input, evaluation, and structural output have resource bounds.

The interactive session does not yet implement the complete language or console
command set, durable table execution, watches, or remote transport. Unsupported
reference workflows report that execution is unavailable. Planner
tests alone do not establish reference-database execution.
Input is currently line-oriented. Submissions are checked against the session's
types and imports before execution. A type error or execution failure leaves
both the retained declarations and last successful result unchanged. Project
imports expose public declarations; private implementation helpers remain
available only inside their defining modules.

Session close seals child admission, cancels only unfinished children owned by
that root session, joins terminal cleanup, then closes transport. A failed
cleanup keeps admission sealed and can be retried; repeated completed close is
deterministic.

## Checks

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
git diff --check -- orna-cli-v1
```
