# ORNA-TASK / ORNA-CONCUR Linked-Test Execution Evidence

Release epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)

Tested repository HEAD: `17122e127438348bd9a0cfa0f070f27861bc4944`

Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`

`source/10-execution.md` SHA-256: `3c194277ae2328059bd2fbc9733a09d0cf38415fa84c080d6879f8d21a4dada4`

`tests/requirement-evidence.json` SHA-256: `153c4c0f9678c52338dfd847e7aa706d888c9441d3b0924d013a6b7015d7a5aa`

## Verified clause scope and register boundary

Before execution, the clause text was checked in the pinned `source/10-execution.md`:

- `ORNA-CONCUR-001` at line 42: one activation/session owner per asynchronous child; cancel and join unfinished children before owner termination.
- `ORNA-TASK-001` through `ORNA-TASK-004` at lines 73, 75, 77 and 79: cancellation fencing; graceful REPL close ownership; process-loss recovery boundaries; and bounded cancellation checkpoints/adapter limitations.
- `ORNA-CONCUR-002` through `ORNA-CONCUR-006` at lines 85, 87, 89, 91 and 93: parallel result ordering, child transaction independence, parent-activation guidance, race selection/loser cleanup and timeout cancellation/join.

The frozen `tests/requirement-evidence.json` has a generic implementation-conformance obligation, rather than a concrete command, for each of these ten clauses. All ten currently remain `implementation_result: not executed`, `tests[0].status: planned`, and `full_implementation_coverage_claimed: false`. The status-guard test passed and the frozen file was not edited. These selected executions do not satisfy the generic obligations or establish full clause coverage.

## Executed checks

All commands below ran with `--locked --offline` from this worktree at the tested HEAD above. Full stdout/stderr, including compiler output, is captured under `/var/tmp/ornadb-task-concur-evidence/` in the named files. The result lines below are reproduced from those transcripts.

### ORNA-TASK-001: acknowledged cancellation and fencing

Selected runtime tests:

```text
$ cargo test --locked --offline -p orna-runtime-v1 --lib tests::terminal_request_transitions_are_fenced_by_owner_epoch -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-runtime-v1 --lib tests::request_activation_replays_an_acknowledged_cancellation_without_writes -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-runtime-v1 --test activation_owner_fence -- --nocapture
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

Transcripts: `task-001-terminal-fence.log`, `task-001-acknowledged-cancel.log`, `task-001-activation-owner-fence.log`.

### ORNA-TASK-002: graceful close behavior

Selected REPL/session-state tests:

```text
$ cargo test --locked --offline -p orna-cli-v1 --bin orna-cli-v1 tests::close_cancels_only_owned_unfinished_children_then_drains_and_is_idempotent -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 64 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-cli-v1 --bin orna-cli-v1 tests::failed_close_keeps_the_session_closed_to_new_children_until_cleanup_retries -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 64 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-cli-v1 --bin orna-cli-v1 tests::failed_child_cancellation_still_attempts_every_owned_child -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 64 filtered out
EXIT_CODE=0
```

Transcripts: `task-002-repl-close.log`, `task-002-close-failure-retry.log`, `task-002-cancel-every-child.log`. These tests exercise selected REPL close state transitions; they do not establish behavior for every host/session adapter or independently launched run.

### ORNA-TASK-003: takeover and recovery

Selected runtime tests:

```text
$ cargo test --locked --offline -p orna-runtime-v1 --lib tests::takeover_recovery_barrier_survives_reopen_until_running_rows_recover -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-runtime-v1 --lib tests::runtime_owned_activation_failure_rolls_back_and_recovers_as_proven_after_reopen -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out
EXIT_CODE=0
```

Transcripts: `task-003-recovery-barrier.log`, `task-003-rollback-recovery.log`. These are state reopen/takeover tests. They do not simulate killing a separate operating-system process, and they do not claim that cleanup runs in a dying process.

### ORNA-TASK-004: bounded cancellation checkpoints

Selected runtime and evaluator tests:

```text
$ cargo test --locked --offline -p orna-runtime-v1 --lib tests::request_cancellation_is_terminal_and_bounded -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-evaluator-v1 --lib tests::relation_scan_checks_cancellation_before_each_page -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 66 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-evaluator-v1 --lib tests::relation_scan_checks_cancellation_before_each_row_callback -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 66 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-evaluator-v1 --lib tests::buffered_relation_stages_check_cancellation_before_each_value -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 66 filtered out
EXIT_CODE=0
```

Transcripts: `task-004-runtime-cancel.log`, `task-004-evaluator-page-checkpoint.log`, `task-004-evaluator-row-checkpoint.log`, `task-004-evaluator-buffered-checkpoint.log`. These exercise selected checkpoints, not every VM, stream, external adapter, or noncooperative worker limitation.

### ORNA-CONCUR-001: supervisor cancellation and join

The live regression target passed:

```text
$ cargo test --locked --offline -p orna-live-v1 --test v1_review_regressions -- --nocapture
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

Transcript: `concur-001-live-regressions-actual.log`. The selected suite includes later-session draining after failed lease acknowledgement, recursive acknowledgement waiting, and reporting an unacknowledged lease.

The first attempted invocation added `--ignored`, selected zero tests, and is retained as `concur-001-live-regressions.log`:

```text
$ cargo test --locked --offline -p orna-live-v1 --test v1_review_regressions -- --ignored --nocapture
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out
EXIT_CODE=0
```

That zero-test invocation is not counted as evidence; the normal invocation above is the executed run.

### ORNA-CONCUR-002, ORNA-CONCUR-005 and ORNA-CONCUR-006: semantic fixture checks

Each selected test loads a checked-in real `.orna` fixture with `include_str!` in `crates/orna-semantic-v1/tests/semantic_graph.rs`. These results establish semantic admission/type diagnostics only; none executes the runtime combinator.

```text
$ cargo test --locked --offline -p orna-semantic-v1 --test semantic_graph parallel_callbacks_share_result_type_and_return_ordered_stream -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 163 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-semantic-v1 --test semantic_graph race_callbacks_share_result_type_and_return_that_type -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 163 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-semantic-v1 --test semantic_graph timeout_callback_and_duration_are_admitted_and_checked -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 163 filtered out
EXIT_CODE=0
```

Transcripts: `concur-002-parallel-semantic-fixture.log`, `concur-005-race-semantic-fixture.log`, `concur-006-timeout-semantic-fixture.log`. The parallel test checks common callback result typing and the declared stream type, not completion-order behavior. The race and timeout tests check callback/duration admission and result typing, not winner selection, loser cleanup, timeout cancellation, or join.

No focused runtime combinator execution was recorded for `ORNA-CONCUR-003` or `ORNA-CONCUR-004`. The following bounded source search found no direct `std.concurrent` or `concurrent.parallel/race/timeout` spelling in the selected execution-package directories (exit 1 means no matches in that search); the fixture search found only the three semantic fixtures listed above (exit 0):

```text
$ rg -n 'std\.concurrent|concurrent\.(parallel|race|timeout)' crates/orna-evaluator-v1/src crates/orna-runtime-v1/src crates/orna-live-v1/src crates/orna-application-v1/src crates/orna-standard/src
PRODUCTION_SEARCH_EXIT_CODE=1
$ rg -n 'std\.concurrent|concurrent\.(parallel|race|timeout)' crates --glob '*.orna'
crates/orna-semantic-v1/tests/fixtures/parallel-callback-results.orna:5:pub fn ordered() = std.concurrent.parallel([number_one, number_two]);
crates/orna-semantic-v1/tests/fixtures/parallel-callback-results.orna:6:pub fn incompatible() = std.concurrent.parallel([number_one, text_result]);
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:4:pub fn completed() = std.concurrent.timeout(answer, 5.s);
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:5:pub fn named() = std.concurrent.timeout(callback: answer, duration: 5.s);
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:6:pub fn bad_callback() = std.concurrent.timeout(42, 5.s);
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:7:pub fn parameterized_callback() = std.concurrent.timeout(needs_input, 5.s);
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:8:pub fn bad_duration() = std.concurrent.timeout(answer, "soon");
crates/orna-semantic-v1/tests/fixtures/timeout-callback-results.orna:9:pub fn bad_shape() = std.concurrent.timeout(answer);
crates/orna-semantic-v1/tests/fixtures/race-callback-results.orna:5:pub fn winner() = std.concurrent.race([number_one, number_two]);
crates/orna-semantic-v1/tests/fixtures/race-callback-results.orna:6:pub fn incompatible() = std.concurrent.race([number_one, text_result]);
FIXTURE_SEARCH_EXIT_CODE=0
```

These search results are limited to the named source directories and checked-in `.orna` files. They are not proof that no runtime implementation or other test exists elsewhere, and no `ORNA-CONCUR-003/004` behavior is claimed.

### Frozen-register status guard

The reference-corpus test verifies that authoritative not-executed markers remain in place:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus requirement_evidence_keeps_all_authoritative_not_executed_markers -- --exact --nocapture
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 43 filtered out
EXIT_CODE=0
```

Transcript: `register-status-preserved.log`. All captured test invocations on this rebased HEAD passed; the only zero-test attempt is separately identified above. No conformance class or full implementation coverage is claimed.
