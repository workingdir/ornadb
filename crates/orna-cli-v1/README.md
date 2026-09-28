# orna-cli-v1

A developing CLI for the current Orna 1.0 source language.

## Command surface

This binary currently accepts `init`, `status`, `fetch`, `diff`, `check`,
`invoke`, `explain`, `repl`, and `run`, plus help and version options. The
available command groups and option forms are listed by `orna --help`; that
list describes this bounded implementation, not the complete Orna 1.0
command set.

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
