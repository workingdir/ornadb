# LiveHost Eval conformance proof boundary

Audit revision: `dc0fcea9201cfb26cd50c14f2279ed133d4b1fa8` on 2026-09-28.
This is a scoped proof report, not a claim that the full remote-Eval
conformance requirement has passed.
Reference publication: `Orna-1.0.0.md`, SHA-256
`d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d`.

## Normative scope

The frozen Orna 1.0.0 reference defines:

- `ORNA-EVAL-001` (`source/14-pages.md:70`): source runs only in explicit
  `eval` or `watch`; ordinary protocol values stay data.
- `ORNA-EVAL-002` (`source/14-pages.md:72`): remote source uses the same
  parser, resolver, type checker, language implementation, module graph,
  activation semantics, diagnostics, and presentation as the local REPL.
- `ORNA-EVAL-003` (`source/14-pages.md:74`): one Eval input is one activation
  transaction.
- `ORNA-TXN-001/002` (`source/10-execution.md:7-9`): successful activation
  writes commit together; escaping errors roll them back.
- `ORNA-CONF-002/003` (`source/01-scope.md:52-54`): report the tests actually
  run, and do not claim a conformance class without its applicable fixtures
  and behavioral tests.

## Focused evidence on current main

The following existing tests pass at the audit revision. The source-backed
application test loads its program from the checked-in
`crates/orna-application-v1/tests/fixtures/admitted-source-transaction.orna`
through `include_str!`; no Orna source is embedded in this report.

```text
CARGO_TARGET_DIR=/home/pbox/.cache/ornadb-eval-proof-target CARGO_BUILD_JOBS=4 cargo test --locked --offline -p orna-application-v1 --test admitted_source_transaction admitted_source_table_mutation_commits_rolls_back_and_replays_terminally

running 1 test
test admitted_source_table_mutation_commits_rolls_back_and_replays_terminally ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
[captured exit code: 0]
```

The LiveHost test separately verifies request-owned transaction commit,
rollback after a table-write fault, and terminal replay without a second
application call. Its test double supplies `TableMutation` directly; it does
not parse or execute source.

```text
CARGO_TARGET_DIR=/home/pbox/.cache/ornadb-eval-proof-target CARGO_BUILD_JOBS=4 cargo test --locked --offline -p orna-live-v1 --test live_host durable_transactional_eval_commits_or_rolls_back_and_replays_terminally

running 1 test
test durable_transactional_eval_commits_or_rolls_back_and_replays_terminally ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 110 filtered out
[captured exit code: 0]
```

## Deferral and captured search evidence

Current main has a source-aware `ApplicationLiveAdapter` implementing
`LiveApplication` (`crates/orna-application-v1/src/lib.rs:1229`), and
`DurableTransactionalEvaluator::execute_source_request` remains a separate
conformance-harness API (`crates/orna-conformance-v1/src/semantic_adapter.rs:2291`).
The source-backed application test directly calls the adapter's
`eval_with_transaction` method with `Message::Eval` and a runtime activation
context. It does not instantiate `LiveHost` or call `dispatch_frame`; the
focused search below returned no matches (exit 1, meaning no match):

```text
rg -n 'LiveHost|dispatch_frame|prepare_application_frame' crates/orna-application-v1/tests/admitted_source_transaction.rs
[no matches; search exit code: 1]
```

`crates/orna-live-v1/tests/live_host.rs:606-640` uses a synthetic
`TransactionalApplication`; it cannot supply the source-to-runtime half. Thus
the two passing tests establish the separate source-adapter and LiveHost
transaction properties, but not their end-to-end composition. This report does
not claim full `ORNA-EVAL-001/002/003` or a conformance class under
`ORNA-CONF-003`.

**Deferred proof:** add a dependency-compatible LiveHost integration test that
passes `ApplicationLiveAdapter` through the real host dispatch and executes the
checked-in `.orna` fixture, then asserts commit, rollback, and terminal replay.
The current accepted proof-file lease is
`work/crates/orna-live-v1/tests/live_host.rs`; its protected worktree is already
dirty, so this report does not modify it. The integration-test path is outside
that exact lease and needs explicit ownership before attempting the combined
proof.
