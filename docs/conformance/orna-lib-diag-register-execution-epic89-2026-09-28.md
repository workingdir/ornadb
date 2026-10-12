# ORNA-LIB / ORNA-DIAG execution note — epic #89

Progress evidence for Beads epic `ornadb-1787968123319-16-24513f57` (GitHub #89). This records selected repository tests actually run on 2026-09-28. It does not claim full clause conformance.

## Verified reference clauses

Locations are in the frozen reference at `/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/`.

| Clause | Location | Scope stated by the clause |
|---|---|---|
| ORNA-LIB-001–003 | `09-standard-library.md:11,13,15` | Standard-library module and collection operations. |
| ORNA-LIB-004–005 | `09-standard-library.md:44,46` | Table operations. |
| ORNA-LIB-006 | `09-standard-library.md:77` | Stream operations. |
| ORNA-LIB-007 | `09-standard-library.md:89` | Standard-library source/import behavior. |
| ORNA-DIAG-001–002 | `18-cli.md:74,76` | User-facing diagnostic output and guidance. |

The frozen `tests/requirement-evidence.json` entries for these nine requirements still say `implementation_result: not executed`; each has only a generic planned implementation-conformance obligation, no executable test IDs, and `full_implementation_coverage_claimed: false`. That frozen reference file was not changed. The test selection below is repository evidence, not a set of tests linked by that register.

## Executed evidence

All successful and failed commands below used the checked-out repository at this branch. Full captured output is retained at the listed local log path.

| Command / scope | Actual result | Captured output |
|---|---:|---|
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-standard -- --nocapture` | 2 passed, exit 0 | `/var/tmp/ornadb-lib-diag-standard.log` |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-evaluator-v1 --test evaluator std_ -- --nocapture` | 81 passed, 149 filtered, exit 0 | `/var/tmp/ornadb-lib-diag-evaluator-allstd.log` |
| `cargo test -p orna-compiler standard -- --nocapture` | 125 passed, 312 filtered, exit 0 | `/var/tmp/ornadb-lib-diag-compiler-standard.log` |
| `cargo test -p orna-table-v1 -- --nocapture` | 38 passed, exit 0 | `/var/tmp/ornadb-lib-diag-table.log` |
| `cargo test -p orna-stream-v1 -- --nocapture` | 33 passed, exit 0 | `/var/tmp/ornadb-lib-diag-stream.log` |
| `cargo test -p orna-conformance-v1 --test transactional_source parsed_ -- --nocapture` | 72 passed, 23 filtered, exit 0 | `/var/tmp/ornadb-lib-diag-transactional.log` |
| `cargo test -p orna-cli-v1 --test diagnostic_output -- --nocapture` from the initial nested worktree | compile failed, exit 101: the test's `include_str!` reference path did not resolve from that checkout depth | `/var/tmp/ornadb-diag-cli-output.log` |
| Same `diagnostic_output` command after moving the clean worktree to a direct child of the repository | 1 passed, exit 0 | `/var/tmp/ornadb-diag-cli-output-relocated.log` |
| `cargo test -p orna-cli-v1 --test project_runtime binary_explain_returns_extended_orna_diagnostic_guidance -- --exact --nocapture` | 1 passed, exit 0 | `/var/tmp/ornadb-diag-cli-explain.log` |
| `cargo test -p orna-conformance-v1 --test semantic_runtime_adapter project_stream_checkpoint_restarts_by_position_and_binds_list_contents -- --exact --nocapture` | failed, exit 101 | `/var/tmp/ornadb-lib-diag-list-source.log` |
| `cargo test -p orna-semantic-v1 -- --nocapture` | 62 passed, 1 failed, exit 101 | `/var/tmp/ornadb-lib-diag-semantic.log` |

## Failures preserved verbatim

The adapter test's subprocess commit was rejected by the configured identity proxy:

```text
[git-commit-proxy] blocked: user.email value 'test@example.invalid' is not in the identity allow-list

thread 'project_stream_checkpoint_restarts_by_position_and_binds_list_contents' (2672597) panicked at crates/orna-conformance-v1/tests/semantic_runtime_adapter.rs:81:9:
assertion failed: Command::new("git").args(arguments).current_dir(temp.path()).status().expect("git command").success()
test project_stream_checkpoint_restarts_by_position_and_binds_list_contents ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 68 filtered out
error: test failed, to rerun pass `-p orna-conformance-v1 --test semantic_runtime_adapter`
cargo_test_exit=101
```

The semantic inventory test found a count mismatch:

```text
thread 'system_api::tests::complete_checked_in_inventory_is_loaded_exactly' panicked at crates/orna-semantic-v1/src/system_api.rs:1829:9:
assertion `left == right` failed
  left: Inventory { singletons: 4, opaque_identifiers: 21, reference_aliases: 78, value_types: 35, enums: 44, relations: 78, functions: 66, failure_codes: 46 }
 right: Inventory { singletons: 4, opaque_identifiers: 21, reference_aliases: 78, value_types: 34, enums: 44, relations: 78, functions: 66, failure_codes: 46 }
test system_api::tests::complete_checked_in_inventory_is_loaded_exactly ... FAILED
test result: FAILED. 62 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
cargo_test_exit=101
```

## Bounded conclusion

These selected tests provide partial implementation evidence for standard-library operations and CLI diagnostics. The adapter case remains unverified because its subprocess commit was blocked by the proxy; the semantic suite has the inventory failure above. The frozen register has no executable linked test IDs for these clauses, so this run does not complete its planned obligations and supports no full-coverage or conformance claim. No reference files, test sources, or product code were changed in this evidence-only slice.
