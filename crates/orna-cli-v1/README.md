# orna-cli-v1

A bounded Rust CLI/REPL/reference-workflow slice for OrnaDB.

It parses only `repl`, `init`, and `run seed|exercise|sensors.ingest`.
The typed planner makes completed workflow postconditions no-ops, while direct
`seed` retains the reference database's duplicate-key failure semantics.

## Boundary

This crate does not open repositories, execute Orna source, connect to an
endpoint, manage credentials, or call a sensor provider. `SessionAdapter` is
the explicit integration seam for authenticated transport/runtime work. The
binary reports plans only and never claims provider or repository execution.

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
