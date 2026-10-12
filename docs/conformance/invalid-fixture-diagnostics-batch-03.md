# Invalid-fixture diagnostics: batch 03

Issue: `ornadb-6u7c` (GitHub #2067). Continuation of `ornadb-t9lv` / `invalid-fixture-diagnostics-batch-02.md`.
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.
Batch rule: first 20 entries of batch 02's pinned remainder, ordered by descending frozen fixture byte size and then path. Fixture sizes below were measured directly from the frozen `examples/invalid/` files.

## Normative basis and interpretation

**ORNA-TEST-002** (`source/32-conformance.md:19`) requires each invalid fixture to identify its expected failing phase and stable diagnostic code. Expected values below come from each fixture's frozen `tests/expected-diagnostics/*.orna.json` entry. The CLI report's `passed` flag verifies the adapter's stable-code comparison as well as the declared phase; its serialized `diagnostic.code` is the underlying analyzer code. For typecheck fixtures with analyzer code `ORNA-S021-TYPE`, the semantic adapter maps the actual diagnostic message to the stable public code shown as observed below.

| Fixture | Bytes | Expected phase / stable code | Observed phase / stable code | Analyzer code | Result |
|---|---:|---|---|---|---|
| `examples/invalid/legacy-colon-bound.orna` | 61 | parse / `ORNA091-E-BOUND-COLON` | parse / `ORNA091-E-BOUND-COLON` | `ORNA091-E-BOUND-COLON` | PASS |
| `examples/invalid/assignment-expression.orna` | 59 | parse / `E1301` | parse / `E1301` | `E1301` | PASS |
| `examples/invalid/legacy-failure-resolve-method.orna` | 58 | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-assert-pipe-bang.orna` | 57 | parse / `ORNA-A091-010` | parse / `ORNA-A091-010` | `ORNA-A091-010` | PASS |
| `examples/invalid/legacy-failure-replay-method.orna` | 57 | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/cross-database-write.orna` | 55 | typecheck / `E9001` | typecheck / `E9001` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/key-update.orna` | 55 | typecheck / `E3002` | typecheck / `E3002` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-field-check.orna` | 55 | parse / `ORNA091-E-FIELD-CONSTRAINT` | parse / `ORNA091-E-FIELD-CONSTRAINT` | `ORNA091-E-FIELD-CONSTRAINT` | PASS |
| `examples/invalid/range-key-overlap-magic.orna` | 55 | typecheck / `E3010` | typecheck / `E3010` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/assert-owner-type-mismatch.orna` | 53 | typecheck / `ORNA-A091-004` | typecheck / `ORNA-A091-004` | `ORNA-A091-004` | PASS |
| `examples/invalid/legacy-pipe-lambda.orna` | 53 | parse / `E1011` | parse / `E1011` | `E1011` | PASS |
| `examples/invalid/legacy-stream-retry-method.orna` | 53 | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | typecheck / `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-field-unique.orna` | 51 | parse / `ORNA091-E-FIELD-CONSTRAINT` | parse / `ORNA091-E-FIELD-CONSTRAINT` | `ORNA091-E-FIELD-CONSTRAINT` | PASS |
| `examples/invalid/legacy-var.orna` | 51 | parse / `ORNA091-E-VAR` | parse / `ORNA091-E-VAR` | `ORNA091-E-VAR` | PASS |
| `examples/invalid/calendar-bucket-no-zone.orna` | 50 | typecheck / `E6001` | typecheck / `E6001` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-sys-storage-call.orna` | 50 | typecheck / `ORNA100-E-SYS-STORAGE-CALL` | typecheck / `ORNA100-E-SYS-STORAGE-CALL` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/legacy-ingest.orna` | 49 | parse / `E1004` | parse / `E1004` | `E1004` | PASS |
| `examples/invalid/mutate-sys-commit.orna` | 49 | typecheck / `E7001` | typecheck / `E7001` | `ORNA-S021-TYPE` | PASS |
| `examples/invalid/static-protocol-function.orna` | 49 | parse / `ORNA091-E-STATIC-FN` | parse / `ORNA091-E-STATIC-FN` | `ORNA091-E-STATIC-FN` | PASS |
| `examples/invalid/top-level-on.orna` | 49 | parse / `E1005` | parse / `E1005` | `E1005` | PASS |

All 20 selected fixtures matched. No mismatch or code/lease attribution was needed, and no production change was made.

## Captured CLI execution

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo run --locked --offline -p orna-conformance-v1 -- --profile bounded-expression-runtime
```

The captured profile report contained 167 fixtures and exited **1**. Six failures were outside this batch: valid fixtures `automatic-failure-propagation.orna`, `failure-natural-key.orna`, `finite-stream.orna`, and `stream-admin-repl.orna`; invalid fixtures `legacy-result.orna` and `two-durable-sources.orna`. They remain verbatim CLI failures and are not attributed to the selected 20. Raw command capture: `/tmp/ornadb-6u7c-batch03-cli.json` (build stderr followed by the CLI JSON report); parsed report: `/tmp/ornadb-6u7c-batch03-report.json`.

Selected fixture results extracted from that real CLI report:

```text
20 selected invalid fixtures; 20 matched; 0 mismatched
CLI exit code: 1 (the six unrelated profile failures listed above)
```

## Pinned remaining fixtures

The remaining 30 entries from batch 02's pinned 50-fixture remainder, in descending byte-size order and path tie-break order:

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
