# Invalid-fixture diagnostics: batch 02

Issue: `ornadb-t9lv` (invalid fixture diagnostics batch two).
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.
CLI execution base: `c5077cb79895864e23755bc66f7b09212d723adb` (`origin/main` at task start). The task branch was then fast-forwarded to `390b259a60c1a9b73bef2e4d9aea76240b34136f`; that intervening commit only adds `crates/orna-conformance-v1/tests/valid_fixtures.rs`, with no production behavior change. The focused reference-corpus test was rerun at `390b259a`.
Batch rule: the next 20 largest fixtures from PR #2044’s 70-fixture remainder, descending by frozen-reference byte size; ties sort by path.

## Normative check and execution

**ORNA-TEST-002** (`source/32-conformance.md:19`) requires each invalid fixture to declare its expected failing phase and stable diagnostic code. Expected values below are loaded from each fixture’s `tests/expected-diagnostics/*.json` record. The full bounded runtime profile executed all 80 fixtures at the pinned base; the CLI report marked 78 matches and two failures.

`PASS` means the harness observed the stated phase and its published diagnostic mapping matched the expected stable code. The CLI JSON also serializes the underlying analyzer diagnostic code; where it is a generic internal type code, both values are shown so the public mapping is not confused with the analyzer code.

| Fixture | Bytes | Expected phase / stable code | Observed phase / stable code | Analyzer code | Result |
|---|---:|---|---|---|---|
| `examples/invalid/wrong-field-type.orna` | 88 | typecheck / `E2001` | typecheck / `E2001` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/module-single-table-assertion.orna` | 85 | typecheck / `ORNA-A091-003` | typecheck / `ORNA-A091-003` | `ORNA-A091-003` | PASS |
| `examples/invalid/legacy-match.orna` | 80 | parse / `ORNA091-E-MATCH` | parse / `ORNA091-E-MATCH` | `ORNA091-E-MATCH` | PASS |
| `examples/invalid/legacy-tryfrom.orna` | 80 | resolve / `ORNA091-E-TRYFROM` | resolve / `ORNA091-E-TRYFROM` | `ORNA091-E-TRYFROM` | PASS |
| `examples/invalid/legacy-fact.orna` | 77 | parse / `ORNA-A091-010` | parse / `ORNA-A091-010` | `ORNA-A091-010` | PASS |
| `examples/invalid/legacy-top-level-impl.orna` | 77 | parse / `ORNA091-E-IMPL-FOR` | parse / `ORNA091-E-IMPL-FOR` | `ORNA091-E-IMPL-FOR` | PASS |
| `examples/invalid/float-money-implicit.orna` | 76 | typecheck / `E5103` | typecheck / `E5103` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/computed-field-update.orna` | 75 | typecheck / `E3011` | typecheck / `E3011` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/secret-display.orna` | 72 | typecheck / `E7002` | typecheck / `E7002` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-constraints-block.orna` | 70 | parse / `ORNA-A091-010` | parse / `ORNA-A091-010` | `ORNA-A091-010` | PASS |
| `examples/invalid/legacy-result.orna` | 70 | typecheck / `ORNA091-E-RESULT` | typecheck / `ORNA-S012-UNRESOLVED` | `ORNA-S012-UNRESOLVED` | FAIL (lease blocked) |
| `examples/invalid/legacy-stream-skip-method.orna` | 69 | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/implicit-conversion-chain.orna` | 68 | typecheck / `ORNA091-E-CONVERSION-CHAIN` | typecheck / `ORNA091-E-CONVERSION-CHAIN` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/rekey-auto-id.orna` | 68 | typecheck / `E3013` | typecheck / `E3013` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-assert-else.orna` | 66 | parse / `ORNA-A091-006` | parse / `ORNA-A091-006` | `ORNA-A091-006` | PASS |
| `examples/invalid/relation-equality.orna` | 66 | typecheck / `E4107` | typecheck / `E4107` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/ambiguous-consumer.orna` | 64 | typecheck / `E9101` | typecheck / `E9101` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-currency-declaration.orna` | 64 | parse / `ORNA091-E-CURRENCY` | parse / `ORNA091-E-CURRENCY` | `ORNA091-E-CURRENCY` | PASS |
| `examples/invalid/unparenthesized-lambda-stage.orna` | 64 | parse / `E1204` | parse / `E1204` | `E1204` | PASS |
| `examples/invalid/unknown-field.orna` | 62 | typecheck / `E2003` | typecheck / `E2003` | `ORNA-S021-TYPE` | PASS |

Nineteen fixtures match. `legacy-result.orna` fails in the expected `typecheck` phase with `ORNA-S012-UNRESOLVED` rather than `ORNA091-E-RESULT`. The failure is in the same `crates/orna-conformance-v1/src/semantic_adapter.rs` selector used by batch 01. Its path remains occupied by worktree `/home/pbox/dev/ornadb/omp-domains/conformance-removed-var-1397`, branch `conformance-removed-var-1397`, HEAD `672dd4b3` (PR/issue #1397). That clean worktree branch carries a different version of the same file; it was left untouched. The current selector omits `ORNA091-E-RESULT` from `typecheck_phase`, allowing the unresolved fallback diagnostic to be selected. This is the precise code-path attribution; no source fix was attempted under the overlapping path lease.

## Captured execution

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo run --locked -p orna-conformance-v1 -- --profile bounded-expression-runtime
80 invalid fixtures; 78 matched; failures: invalid/legacy-result.orna, invalid/two-durable-sources.orna
cargo run exit code: 1
```

Raw captured output: `/var/tmp/ornadb-t9lv-batch02-conformance.log`.

## Focused cargo test

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked -p orna-conformance-v1 --test reference_corpus
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
cargo test exit code: 0
```

Captured output: `/var/tmp/ornadb-t9lv-batch02-cargo-test.log`.

## Pinned follow-up fixtures

The remaining 50 frozen-reference invalid fixtures, ranked by size and then path, are deferred to later batches:
- `examples/invalid/legacy-colon-bound.orna` (61 B)
- `examples/invalid/assignment-expression.orna` (59 B)
- `examples/invalid/legacy-failure-resolve-method.orna` (58 B)
- `examples/invalid/legacy-assert-pipe-bang.orna` (57 B)
- `examples/invalid/legacy-failure-replay-method.orna` (57 B)
- `examples/invalid/cross-database-write.orna` (55 B)
- `examples/invalid/key-update.orna` (55 B)
- `examples/invalid/legacy-field-check.orna` (55 B)
- `examples/invalid/range-key-overlap-magic.orna` (55 B)
- `examples/invalid/assert-owner-type-mismatch.orna` (53 B)
- `examples/invalid/legacy-pipe-lambda.orna` (53 B)
- `examples/invalid/legacy-stream-retry-method.orna` (53 B)
- `examples/invalid/legacy-field-unique.orna` (51 B)
- `examples/invalid/legacy-var.orna` (51 B)
- `examples/invalid/calendar-bucket-no-zone.orna` (50 B)
- `examples/invalid/legacy-sys-storage-call.orna` (50 B)
- `examples/invalid/legacy-ingest.orna` (49 B)
- `examples/invalid/mutate-sys-commit.orna` (49 B)
- `examples/invalid/static-protocol-function.orna` (49 B)
- `examples/invalid/top-level-on.orna` (49 B)
- `examples/invalid/comparison-chain.orna` (48 B)
- `examples/invalid/legacy-sys-runtime.orna` (48 B)
- `examples/invalid/question-coalesce-adjacent.orna` (48 B)
- `examples/invalid/affine-sum.orna` (47 B)
- `examples/invalid/legacy-ensure.orna` (47 B)
- `examples/invalid/missing-required-field.orna` (47 B)
- `examples/invalid/legacy-postfix-question.orna` (46 B)
- `examples/invalid/money-float.orna` (46 B)
- `examples/invalid/assert-missing-semicolon.orna` (44 B)
- `examples/invalid/transaction-block.orna` (44 B)
- `examples/invalid/float-key.orna` (43 B)
- `examples/invalid/legacy-empty-closure.orna` (39 B)
- `examples/invalid/record-punning.orna` (39 B)
- `examples/invalid/legacy-return-arrow.orna` (38 B)
- `examples/invalid/legacy-store.orna` (37 B)
- `examples/invalid/legacy-view.orna` (36 B)
- `examples/invalid/legacy-log.orna` (34 B)
- `examples/invalid/legacy-refined-where.orna` (33 B)
- `examples/invalid/top-level-expression.orna` (33 B)
- `examples/invalid/incompatible-dimensions.orna` (32 B)
- `examples/invalid/currency-addition.orna` (31 B)
- `examples/invalid/legacy-opaque.orna` (31 B)
- `examples/invalid/unsafe-row-key-repeat.orna` (31 B)
- `examples/invalid/reserved-std.orna` (30 B)
- `examples/invalid/reserved-sys.orna` (30 B)
- `examples/invalid/row-declaration.orna` (28 B)
- `examples/invalid/affine-addition.orna` (27 B)
- `examples/invalid/assert-empty.orna` (25 B)
- `examples/invalid/question-on-int.orna` (20 B)
- `examples/invalid/module-zero-table-assertion.orna` (19 B)
