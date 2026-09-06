# orna-cli-v1

A developing CLI for the current Orna 1.0 source language.

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
let n = 21;
fn twice(value) = value + value;
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
command set, durable table execution, watches, or remote transport. `init` and
unsupported reference workflows report that execution is unavailable. Planner
tests alone do not establish reference-database execution.
Input is currently line-oriented. Session declarations with type annotations
are rejected pending semantic admission; annotations are never silently ignored.

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
