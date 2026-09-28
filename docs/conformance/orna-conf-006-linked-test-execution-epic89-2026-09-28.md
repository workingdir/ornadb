# ORNA-CONF-006 Linked-Test Execution Record

Epic progress for `ornadb-1787968123319-16-24513f57` (GitHub #89).

Base: `fc3469fe1cc2c24f0ebe68bb45bfd657e62556a0` (`origin/main` when run).

## Frozen criterion and register state

The frozen reference states **ORNA-CONF-006**: “A full-runtime claim MUST
execute the assertion, automatic-failure, recovery, conversion, transaction,
cancellation and cross-table validation scenarios applicable to it.”
Source: `source/01-scope.md:60`.

The SHA-256 of `source/01-scope.md` was
`26b8cc52da70ed17a0efd3fe2ef1b0da4f55a64e689b68e952dab24a0786ef06`.
The frozen `tests/requirement-evidence.json` still records
`implementation_result: not executed`, linked-test `status: planned`, and
`full_implementation_coverage_claimed: false` (SHA-256
`153c4c0f9678c52338dfd847e7aa706d888c9441d3b0924d013a6b7015d7a5aa`).
That reference register was not modified.

`docs/conformance/orna-conf-006-full-runtime-scenario-applicability.md`
records the seven applicable families and representative scenario IDs. It
does not map every indexed scenario to a specific Rust test command. The runs
below therefore record bounded outcomes of existing conformance targets; they
do not claim the complete ORNA-CONF-006 obligation or a full-runtime claim.

## Actual runs

Both commands ran from a clean worktree at the repository depth expected by
the existing reference `include_str!` paths. Complete captured logs were retained
at `/tmp/orna-conf-006-library-tests-captured.log` and
`/tmp/orna-conf-006-runtime-scenarios.log` for this run.

### Conformance library tests

Command:

```text
cargo test --locked --offline -p orna-conformance-v1 --lib -- --nocapture
```

Captured result:

```text
running 54 tests
test result: FAILED. 31 passed; 23 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.58s
error: test failed, to rerun pass `-p orna-conformance-v1 --lib`
cargo_test_exit=101
```

Verbatim failed-test list:

```text
semantic_adapter::durable_tests::application_eval_resolves_admits_stages_and_commits_digest
semantic_adapter::durable_tests::authoritative_five_module_project_executes_seed_and_exercise
semantic_adapter::durable_tests::ordinary_activation_reuses_one_captured_now_for_repeated_writes
semantic_adapter::durable_tests::project_activation_keeps_modules_qualified_and_rolls_back_failed_roots
semantic_adapter::durable_tests::request_source_activation_commits_row_and_terminal_together
semantic_adapter::durable_tests::request_source_failure_replays_without_executing_new_source
semantic_adapter::durable_tests::request_source_never_executes_an_existing_nonterminal_reservation
semantic_adapter::durable_tests::running_table_continuation_commits_once_with_the_exact_success_terminal
semantic_adapter::durable_tests::running_table_continuation_fault_recovers_as_proven
semantic_adapter::durable_tests::running_table_continuation_rejects_foreign_and_stale_capabilities_without_writes
semantic_adapter::durable_tests::running_table_continuation_semantic_terminal_is_absorbing_before_replay
semantic_adapter::durable_tests::semantically_admitted_user_now_shadows_activation_intrinsic
semantic_adapter::durable_tests::source_activation_commits_rows_and_reopens_for_the_next_activation
semantic_adapter::list_stream_tests::handler_failure_does_not_advance_literal_list_checkpoint
semantic_adapter::list_stream_tests::literal_list_stream_changed_contents_do_not_resume_a_named_predecessor
semantic_adapter::list_stream_tests::literal_list_stream_identity_preserves_payload_boundaries
semantic_adapter::list_stream_tests::literal_list_stream_reopens_without_duplicate_delivery
semantic_adapter::list_stream_tests::project_list_stream_changed_contents_select_a_fresh_checkpoint
semantic_adapter::list_stream_tests::project_stream_admits_authoritative_nominal_decimal_composite_readings
semantic_adapter::list_stream_tests::project_stream_assertion_rolls_back_invalid_delivery_and_checkpoint
semantic_adapter::list_stream_tests::project_stream_preserves_inline_literal_source_support
semantic_adapter::list_stream_tests::project_stream_skips_arbitrary_pipeline_without_publishing_rows_or_checkpoint
semantic_adapter::list_stream_tests::project_transaction_route_skips_admitted_stream_root_before_evaluation
```

The captured cause emitted during these failures was:

```text
[git-commit-proxy] blocked: user.email value 'test@example.invalid' is not in the identity allow-list
assertion failed: Command::new("git").args(args).current_dir(path).status().expect("git command").success()
```

### Runtime-scenario integration tests

Command:

```text
cargo test --locked --offline -p orna-conformance-v1 --test runtime_scenarios -- --nocapture
```

Captured result:

```text
running 16 tests
test result: FAILED. 10 passed; 6 failed; 0 ignored; 0 measured; 0 filtered out; finished in 127.27s
error: test failed, to rerun pass `-p orna-conformance-v1 --test runtime_scenarios`
cargo_test_exit=101
```

Verbatim failed-test list:

```text
assert_checkpoint_091_exposes_durable_runtime_adapter_evidence
durable_function_value_table_assertion_commits_and_rolls_back_candidate_rows
durable_source_publication_projects_the_frozen_prefix_into_git
eval_003_replays_the_terminal_outcome_without_a_second_row
published_report_declares_bounded_runtime_adapter_scenarios_without_an_orna_engine_witness
transaction_scenarios_cross_the_durable_runtime_boundary
```

Five failures above that create temporary Git commits emitted the same
`test@example.invalid` proxy rejection and then failed at
`assertion failed: ProcessCommand::new("git").args(args).current_dir(path).status().expect("git command").success()`.
The remaining failure preserved this captured assertion verbatim:

```text
assertion `left == right` failed
  left: ["ASSERT-CHECKPOINT-091", "EVAL-003", "FAIL-001", "LIVE-001", "LIVE-002", "LIVE-003", "LIVE-004", "REPL-001", "SYS-RT-RENAME-100", "TXN-001", "TXN-002"]
 right: ["REPL-001", "TXN-001", "TXN-002", "CP-001", "LIVE-001", "LIVE-002", "LIVE-003", "LIVE-004", "SYS-RT-RENAME-100", "ASSERT-CHECKPOINT-091", "FAIL-001", "EVAL-003"]
```

The linked runtime target passed 10 tests and failed 6; the library target
passed 31 and failed 23. Failures remain failures in this record. No test
identity was overridden to get a different outcome. No test result was
written into the frozen register, and no complete family-coverage or
full-runtime claim is made.

## Initial worktree-layout failure

An initial attempt from a more deeply nested worktree failed before test
execution because these existing relative `include_str!` paths assume the
reference tree is adjacent to the repository checkout. In the deeper worktree,
the compiler reported five instances of:

```text
error: couldn't read `crates/orna-conformance-v1/src/../../../../reference/Orna-1.0.0/examples/reference/library.orna`: No such file or directory (os error 2)
```

The other four fixture filenames were `main.orna`, `sensors.orna`,
`values.orna`, and `warehouse.orna`, with the same error/location pattern.
That attempt stopped before tests ran; its shell exit status was not captured.
The recorded test outcomes above are from the corrected worktree path.
