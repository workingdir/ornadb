# Invalid-fixture diagnostics: batch 01

Issue: `ornadb-xas1` (invalid fixture diagnostics conformance).
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.
Implementation base: `36237a4e2f29e300e672cbda468d8e14444649d8` (`origin/main`).
Batch rule: the ten largest `examples/invalid/*.orna` files by byte size at the frozen reference; ties sort by path.

## Normative check and execution

**ORNA-TEST-002** (`source/32-conformance.md:19`) requires each invalid fixture to identify its expected failing phase and stable diagnostic code. Expected phase/code below come from the matching `tests/expected-diagnostics/*.json` record. The bounded runtime conformance run executed all 80 invalid fixtures at this base: 78 matched their declared phase/code and 2 did not. These rows are the ten selected fixtures from its captured per-fixture output; the second full-corpus mismatch is `legacy-result.orna`, outside this size-ranked batch and included in the follow-up list.

| Fixture | Bytes | Expected phase | Expected code | Observed code | Result |
|---|---:|---|---|---|
| `examples/invalid/effectful-display.orna` | 215 | typecheck | `E7201` | `E7201` | PASS |
| `examples/invalid/two-durable-sources.orna` | 152 | typecheck | `E9102` | `ORNA-S021-TYPE` | FAIL (blocked) |
| `examples/invalid/computed-field-insert.orna` | 122 | typecheck | `E3010` | `E3010` | PASS |
| `examples/invalid/legacy-checkpoint-reset-method.orna` | 111 | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | PASS |
| `examples/invalid/computed-field-effect.orna` | 106 | typecheck | `E3012` | `E3012` | PASS |
| `examples/invalid/duplicate-key.orna` | 105 | evaluate | `E3001` | `E3001` | PASS |
| `examples/invalid/assert-effectful-table.orna` | 96 | typecheck | `ORNA-A091-007` | `ORNA-A091-007` | PASS |
| `examples/invalid/legacy-assert-owner-pipe.orna` | 93 | typecheck | `ORNA-A091-002` | `ORNA-A091-002` | PASS |
| `examples/invalid/legacy-assert-self-pipe.orna` | 93 | typecheck | `ORNA-A091-002` | `ORNA-A091-002` | PASS |
| `examples/invalid/currency-static-symbol.orna` | 90 | typecheck | `ORNA091-E-CURRENCY-SYMBOL` | `ORNA091-E-CURRENCY-SYMBOL` | PASS |

Nine fixtures match their expected phase and stable code. `two-durable-sources.orna` fails in the expected `typecheck` phase but produces `ORNA-S021-TYPE` instead of `E9102`. The current mapping is in `crates/orna-conformance-v1/src/semantic_adapter.rs`. That file is occupied by the separate `conformance-removed-var-1397` worktree/branch (commit `672dd4b3`, issue #1397); its tree is clean, but its branch carries a different version of the same source file. The batch leaves this mismatch for the owner to reconcile after the path lease clears. No unrelated semantic message edit is safe here: an earlier targeted change altered the existing multiple-checkpointed-roots test behavior.

## Focused verification

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked -p orna-conformance-v1 --test reference_corpus
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
exit code: 0
```

Captured output: `/var/tmp/ornadb-xas1-batch01-cargo-test.log` (local run transcript). The current bounded CLI run is captured at `/var/tmp/ornadb-xas1-batch01-conformance.log`; it exited 1 after reporting the two corpus mismatches. The cargo suite loads the reference corpus and exercises the conformance harness; it does not erase the selected stable-code mismatch above.

## Pinned follow-up fixtures

The remaining 70 frozen-reference invalid fixtures are left for later batches, pinned here by reference paths and byte sizes:

- `examples/invalid/affine-addition.orna` (27 B)
- `examples/invalid/affine-sum.orna` (47 B)
- `examples/invalid/ambiguous-consumer.orna` (64 B)
- `examples/invalid/assert-empty.orna` (25 B)
- `examples/invalid/assert-missing-semicolon.orna` (44 B)
- `examples/invalid/assert-owner-type-mismatch.orna` (53 B)
- `examples/invalid/assignment-expression.orna` (59 B)
- `examples/invalid/calendar-bucket-no-zone.orna` (50 B)
- `examples/invalid/comparison-chain.orna` (48 B)
- `examples/invalid/computed-field-update.orna` (75 B)
- `examples/invalid/cross-database-write.orna` (55 B)
- `examples/invalid/currency-addition.orna` (31 B)
- `examples/invalid/float-key.orna` (43 B)
- `examples/invalid/float-money-implicit.orna` (76 B)
- `examples/invalid/implicit-conversion-chain.orna` (68 B)
- `examples/invalid/incompatible-dimensions.orna` (32 B)
- `examples/invalid/key-update.orna` (55 B)
- `examples/invalid/legacy-assert-else.orna` (66 B)
- `examples/invalid/legacy-assert-pipe-bang.orna` (57 B)
- `examples/invalid/legacy-colon-bound.orna` (61 B)
- `examples/invalid/legacy-constraints-block.orna` (70 B)
- `examples/invalid/legacy-currency-declaration.orna` (64 B)
- `examples/invalid/legacy-empty-closure.orna` (39 B)
- `examples/invalid/legacy-ensure.orna` (47 B)
- `examples/invalid/legacy-fact.orna` (77 B)
- `examples/invalid/legacy-failure-replay-method.orna` (57 B)
- `examples/invalid/legacy-failure-resolve-method.orna` (58 B)
- `examples/invalid/legacy-field-check.orna` (55 B)
- `examples/invalid/legacy-field-unique.orna` (51 B)
- `examples/invalid/legacy-ingest.orna` (49 B)
- `examples/invalid/legacy-log.orna` (34 B)
- `examples/invalid/legacy-match.orna` (80 B)
- `examples/invalid/legacy-opaque.orna` (31 B)
- `examples/invalid/legacy-pipe-lambda.orna` (53 B)
- `examples/invalid/legacy-postfix-question.orna` (46 B)
- `examples/invalid/legacy-refined-where.orna` (33 B)
- `examples/invalid/legacy-result.orna` (70 B)
- `examples/invalid/legacy-return-arrow.orna` (38 B)
- `examples/invalid/legacy-store.orna` (37 B)
- `examples/invalid/legacy-stream-retry-method.orna` (53 B)
- `examples/invalid/legacy-stream-skip-method.orna` (69 B)
- `examples/invalid/legacy-sys-runtime.orna` (48 B)
- `examples/invalid/legacy-sys-storage-call.orna` (50 B)
- `examples/invalid/legacy-top-level-impl.orna` (77 B)
- `examples/invalid/legacy-tryfrom.orna` (80 B)
- `examples/invalid/legacy-var.orna` (51 B)
- `examples/invalid/legacy-view.orna` (36 B)
- `examples/invalid/missing-required-field.orna` (47 B)
- `examples/invalid/module-single-table-assertion.orna` (85 B)
- `examples/invalid/module-zero-table-assertion.orna` (19 B)
- `examples/invalid/money-float.orna` (46 B)
- `examples/invalid/mutate-sys-commit.orna` (49 B)
- `examples/invalid/question-coalesce-adjacent.orna` (48 B)
- `examples/invalid/question-on-int.orna` (20 B)
- `examples/invalid/range-key-overlap-magic.orna` (55 B)
- `examples/invalid/record-punning.orna` (39 B)
- `examples/invalid/rekey-auto-id.orna` (68 B)
- `examples/invalid/relation-equality.orna` (66 B)
- `examples/invalid/reserved-std.orna` (30 B)
- `examples/invalid/reserved-sys.orna` (30 B)
- `examples/invalid/row-declaration.orna` (28 B)
- `examples/invalid/secret-display.orna` (72 B)
- `examples/invalid/static-protocol-function.orna` (49 B)
- `examples/invalid/top-level-expression.orna` (33 B)
- `examples/invalid/top-level-on.orna` (49 B)
- `examples/invalid/transaction-block.orna` (44 B)
- `examples/invalid/unknown-field.orna` (62 B)
- `examples/invalid/unparenthesized-lambda-stage.orna` (64 B)
- `examples/invalid/unsafe-row-key-repeat.orna` (31 B)
- `examples/invalid/wrong-field-type.orna` (88 B)
