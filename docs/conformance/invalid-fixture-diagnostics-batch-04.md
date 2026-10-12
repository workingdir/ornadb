# Invalid-fixture diagnostics: batch 04

Issue: `ornadb-i6dl` (final invalid-fixture batch).
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.
Batch rule: final 30 of the frozen 80 `examples/invalid/*.orna` fixtures, following batches 01–03: descending byte size, then path. These are ranks 51–80.

## Normative check and selected fixture results

**ORNA-TEST-002** (`source/32-conformance.md:19`) requires each invalid fixture to identify the expected failing phase and stable diagnostic code. Expectations below are from the matching frozen `tests/expected-diagnostics/*.orna.json` records. `observed stable` reflects the harness stable-code comparison; `analyzer code` is the lower-level diagnostic serialized by the full profile report. The selected fixture is counted as matched only where its failing phase is exact and its expectation is satisfied.

| Rank | Fixture | Bytes | Expected phase / stable code | Observed phase / stable code | Analyzer code | Result |
|---:|---|---:|---|---|---|---|
| 51 | `examples/invalid/comparison-chain.orna` | 48 | parse / `E1302` | parse / `E1302` | `E1302` | PASS |
| 52 | `examples/invalid/legacy-sys-runtime.orna` | 48 | resolve / `ORNA100-E-SYS-RUNTIME` | resolve / `ORNA100-E-SYS-RUNTIME` | `ORNA100-E-SYS-RUNTIME` | PASS |
| 53 | `examples/invalid/question-coalesce-adjacent.orna` | 48 | parse / `ORNA091-E-POSTFIX-QUESTION` | parse / `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | PASS |
| 54 | `examples/invalid/affine-sum.orna` | 47 | typecheck / `E5003` | typecheck / `E5003` | `ORNA-S021-TYPE` | PASS |
| 55 | `examples/invalid/legacy-ensure.orna` | 47 | parse / `ORNA-A091-010` | parse / `ORNA-A091-010` | `ORNA-A091-010` | PASS |
| 56 | `examples/invalid/missing-required-field.orna` | 47 | typecheck / `E2002` | typecheck / `E2002` | `ORNA-S021-TYPE` | PASS |
| 57 | `examples/invalid/legacy-postfix-question.orna` | 46 | parse / `ORNA091-E-POSTFIX-QUESTION` | parse / `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | PASS |
| 58 | `examples/invalid/money-float.orna` | 46 | typecheck / `E5004` | typecheck / `E5004` | `ORNA-S021-TYPE` | PASS |
| 59 | `examples/invalid/assert-missing-semicolon.orna` | 44 | parse / `ORNA-A091-005` | parse / `ORNA-A091-005` | `ORNA-A091-005` | PASS |
| 60 | `examples/invalid/transaction-block.orna` | 44 | parse / `E1007` | parse / `E1007` | `E1007` | PASS |
| 61 | `examples/invalid/float-key.orna` | 43 | typecheck / `E3003` | typecheck / `E3003` | `ORNA-S021-TYPE` | PASS |
| 62 | `examples/invalid/legacy-empty-closure.orna` | 39 | parse / `E1012` | parse / `E1012` | `E1012` | PASS |
| 63 | `examples/invalid/record-punning.orna` | 39 | parse / `E1013` | parse / `E1013` | `E1013` | PASS |
| 64 | `examples/invalid/legacy-return-arrow.orna` | 38 | parse / `ORNA091-E-RETURN-ARROW` | parse / `ORNA091-E-RETURN-ARROW` | `ORNA091-E-RETURN-ARROW` | PASS |
| 65 | `examples/invalid/legacy-store.orna` | 37 | parse / `E1003` | parse / `E1003` | `E1003` | PASS |
| 66 | `examples/invalid/legacy-view.orna` | 36 | parse / `E1002` | parse / `E1002` | `E1002` | PASS |
| 67 | `examples/invalid/legacy-log.orna` | 34 | parse / `E1001` | parse / `E1001` | `E1001` | PASS |
| 68 | `examples/invalid/legacy-refined-where.orna` | 33 | parse / `ORNA-A091-001` | parse / `ORNA-A091-001` | `ORNA-A091-001` | PASS |
| 69 | `examples/invalid/top-level-expression.orna` | 33 | parse / `E1006` | parse / `E1006` | `E1006` | PASS |
| 70 | `examples/invalid/incompatible-dimensions.orna` | 32 | typecheck / `E5001` | typecheck / `E5001` | `ORNA-S021-TYPE` | PASS |
| 71 | `examples/invalid/currency-addition.orna` | 31 | typecheck / `E5003` | typecheck / `E5003` | `ORNA-S021-TYPE` | PASS |
| 72 | `examples/invalid/legacy-opaque.orna` | 31 | parse / `ORNA091-E-OPAQUE` | parse / `ORNA091-E-OPAQUE` | `ORNA091-E-OPAQUE` | PASS |
| 73 | `examples/invalid/unsafe-row-key-repeat.orna` | 31 | row-validation / `E3004` | row-validation / `E3004` | `E3004` | PASS |
| 74 | `examples/invalid/reserved-std.orna` | 30 | typecheck / `E1009` | typecheck / `E1009` | `ORNA-S021-TYPE` | PASS |
| 75 | `examples/invalid/reserved-sys.orna` | 30 | typecheck / `E1008` | typecheck / `E1008` | `ORNA-S021-TYPE` | PASS |
| 76 | `examples/invalid/row-declaration.orna` | 28 | parse / `E8001` | parse / `E8001` | `E8001` | PASS |
| 77 | `examples/invalid/affine-addition.orna` | 27 | typecheck / `E5002` | typecheck / `E5002` | `ORNA-S021-TYPE` | PASS |
| 78 | `examples/invalid/assert-empty.orna` | 25 | parse / `ORNA-A091-011` | parse / `ORNA-A091-011` | `ORNA-A091-011` | PASS |
| 79 | `examples/invalid/question-on-int.orna` | 20 | parse / `ORNA091-E-POSTFIX-QUESTION` | parse / `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | PASS |
| 80 | `examples/invalid/module-zero-table-assertion.orna` | 19 | typecheck / `ORNA-A091-012` | typecheck / `ORNA-A091-012` | `ORNA-A091-012` | PASS |

All 30 selected fixtures match the frozen expected phase and stable diagnostic. Generic analyzer code `ORNA-S021-TYPE` remains separately visible for semantic fixtures; `passed=true` and the selected-stage `expectation_satisfied=true` confirm the expected stable-code mapping. No selected fixture was marked mismatched.

## Full CLI capture (exit status preserved)

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo run --locked --offline -p orna-conformance-v1 -- --profile bounded-expression-runtime
```

The full profile emitted 167 fixture records and exited **1**, preserved as-is. It reports six unrelated profile failures: `valid/automatic-failure-propagation.orna` resolve / `ORNA-S012-UNRESOLVED`; `valid/failure-natural-key.orna`, `valid/finite-stream.orna`, and `valid/stream-admin-repl.orna` typecheck / `ORNA-S021-TYPE`; and the two previously recorded invalid-fixture mismatches detailed below. The 30 selected fixtures all pass their frozen diagnostic contract. Full raw build and CLI transcript: `/tmp/ornadb-i6dl-full-cli.log`; captured CLI exit code: `1`.

## Prior mismatch attribution and lease

| Fixture | Frozen expectation | Current observation | Contract attribution |
|---|---|---|---|
| `legacy-result.orna` | typecheck / `ORNA091-E-RESULT` | typecheck / `ORNA-S012-UNRESOLVED` | ORNA-ERR-009 (`source/06-expressions.md:409`) requires the removed Result/Ok/Err surface to receive `ORNA091-E-RESULT`; expectation is pinned by `tests/expected-diagnostics/legacy-result.orna.json`. |
| `two-durable-sources.orna` | typecheck / `E9102` | typecheck / `ORNA-S021-TYPE` | ORNA-CONSUMER-005 (`source/11-streams.md:40`) requires a diagnostic for multiple checkpointed source roots; expectation is pinned by `tests/expected-diagnostics/two-durable-sources.orna.json`. |

Both observations are outside this final batch and remain included in the full-profile exit status. The relevant adapter path, `crates/orna-conformance-v1/src/semantic_adapter.rs`, is leased by worktree `/home/pbox/dev/ornadb/omp-domains/conformance-removed-var-1397`, branch `conformance-removed-var-1397`, HEAD `672dd4b3c62eee97f0a2e8c45a6b648964bae2e3` (PR #1397); that worktree was clean at inspection. No overlapping source change was made. The expected codes and violated clauses are listed above; no replacement diagnostic behavior is inferred.

## Focused green capture

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus
running 45 tests
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
cargo test exit code: 0
```

Captured Cargo transcript: `/tmp/ornadb-i6dl-reference-corpus.log`.

## Final corpus totals

| Batch | Matched | Mismatched |
|---|---:|---:|
| 01 (10 fixtures) | 9 | 1 |
| 02 (20 fixtures) | 19 | 1 |
| 03 (20 fixtures) | 20 | 0 |
| 04 (30 fixtures) | 30 | 0 |
| **Complete frozen corpus (80)** | **78** | **2** |

The two mismatches are `legacy-result.orna` and `two-durable-sources.orna`; both remain attributed to the occupied adapter path above. This report completes execution and evidence for all 80 frozen invalid fixtures without claiming either mismatch passed.
