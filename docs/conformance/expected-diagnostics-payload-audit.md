# Expected diagnostics sidecar payload audit

Issue: `orna-conformance-v1 expected_diagnostics_payload_audit` (Expected diagnostics sidecar deep verification).
Reference: `/home/kieran/dev/ornadb/reference/Orna-1.1.0`.
Implementation base: `dc1a82f6d` (`origin/main` at worktree creation).

## Scope and method

**ORNA-TEST-002** (`source/32-conformance.md:19`) requires each invalid fixture to state its failing phase and stable diagnostic code. This audit directly ran all 80 frozen-reference invalid sources through the `SemanticAdapter` stage declared by their sidecar, then compared emitted stage, public stable code, and message against each sidecar and its conformance-manifest row. Sources and sidecars are loaded with `include_str!` in the focused integration test. The postfix `?` sidecar expectation was updated to the exact Orna 1.1.0 diagnostic wording; every sidecar remains checksum-tracked.

All 80 sidecars have exactly these fields: `version`, `fixture`, `failing_phase`, `primary_diagnostic`, `message_contains`, and `status`. All declare version `1.0.0` and status `expected-not-executed`; no sidecar has `spans`, `message_class`, severity, or another message field. Therefore spans and a separate message class have no expected value to compare. In the emitted payload, `severity` is `error` and `spans` is `[]` for every fixture. `primary_diagnostic` is compared to the adapter’s published stable code; the lower-level diagnostic code is also recorded because typechecker-backed mapped diagnostics commonly emit native `ORNA-S021-TYPE` while the adapter publishes the fixture’s stable code. `message_contains` is checked as the substring contract it names.

## Per-fixture results

| Fixture | Phase | Expected stable code | Emitted stable code | Message check | Payload result |
|---|---|---|---|---|---|
| `examples/invalid/affine-addition.orna` | typecheck | `E5002` | `E5002` | substring match | MATCH |
| `examples/invalid/affine-sum.orna` | typecheck | `E5003` | `E5003` | substring match | MATCH |
| `examples/invalid/ambiguous-consumer.orna` | typecheck | `E9101` | `E9101` | substring match | MATCH |
| `examples/invalid/assert-effectful-table.orna` | typecheck | `ORNA-A091-007` | `ORNA-A091-007` | substring match | MATCH |
| `examples/invalid/assert-empty.orna` | parse | `ORNA-A091-011` | `ORNA-A091-011` | substring match | MATCH |
| `examples/invalid/assert-missing-semicolon.orna` | parse | `ORNA-A091-005` | `ORNA-A091-005` | substring match | MATCH |
| `examples/invalid/assert-owner-type-mismatch.orna` | typecheck | `ORNA-A091-004` | `ORNA-A091-004` | substring match | MATCH |
| `examples/invalid/assignment-expression.orna` | parse | `E1301` | `E1301` | substring match | MATCH |
| `examples/invalid/calendar-bucket-no-zone.orna` | typecheck | `E6001` | `E6001` | substring match | MATCH |
| `examples/invalid/comparison-chain.orna` | parse | `E1302` | `E1302` | substring match | MATCH |
| `examples/invalid/computed-field-effect.orna` | typecheck | `E3012` | `E3012` | substring match | MATCH |
| `examples/invalid/computed-field-insert.orna` | typecheck | `E3010` | `E3010` | substring match | MATCH |
| `examples/invalid/computed-field-update.orna` | typecheck | `E3011` | `E3011` | substring match | MATCH |
| `examples/invalid/cross-database-write.orna` | typecheck | `E9001` | `E9001` | substring match | MATCH |
| `examples/invalid/currency-addition.orna` | typecheck | `E5003` | `E5003` | substring match | MATCH |
| `examples/invalid/currency-static-symbol.orna` | typecheck | `ORNA091-E-CURRENCY-SYMBOL` | `ORNA091-E-CURRENCY-SYMBOL` | substring match | MATCH |
| `examples/invalid/duplicate-key.orna` | evaluate | `E3001` | `E3001` | substring match | MATCH |
| `examples/invalid/effectful-display.orna` | typecheck | `E7201` | `E7201` | substring match | MATCH |
| `examples/invalid/float-key.orna` | typecheck | `E3003` | `E3003` | substring match | MATCH |
| `examples/invalid/float-money-implicit.orna` | typecheck | `E5103` | `E5103` | substring match | MATCH |
| `examples/invalid/implicit-conversion-chain.orna` | typecheck | `ORNA091-E-CONVERSION-CHAIN` | `ORNA091-E-CONVERSION-CHAIN` | substring match | MATCH |
| `examples/invalid/incompatible-dimensions.orna` | typecheck | `E5001` | `E5001` | substring match | MATCH |
| `examples/invalid/key-update.orna` | typecheck | `E3002` | `E3002` | substring match | MATCH |
| `examples/invalid/legacy-assert-else.orna` | parse | `ORNA-A091-006` | `ORNA-A091-006` | substring match | MATCH |
| `examples/invalid/legacy-assert-owner-pipe.orna` | typecheck | `ORNA-A091-002` | `ORNA-A091-002` | substring match | MATCH |
| `examples/invalid/legacy-assert-pipe-bang.orna` | parse | `ORNA-A091-010` | `ORNA-A091-010` | substring match | MATCH |
| `examples/invalid/legacy-assert-self-pipe.orna` | typecheck | `ORNA-A091-002` | `ORNA-A091-002` | substring match | MATCH |
| `examples/invalid/legacy-checkpoint-reset-method.orna` | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | substring match | MATCH |
| `examples/invalid/legacy-colon-bound.orna` | parse | `ORNA091-E-BOUND-COLON` | `ORNA091-E-BOUND-COLON` | substring match | MATCH |
| `examples/invalid/legacy-constraints-block.orna` | parse | `ORNA-A091-010` | `ORNA-A091-010` | substring match | MATCH |
| `examples/invalid/legacy-currency-declaration.orna` | parse | `ORNA091-E-CURRENCY` | `ORNA091-E-CURRENCY` | substring match | MATCH |
| `examples/invalid/legacy-empty-closure.orna` | parse | `E1012` | `E1012` | substring match | MATCH |
| `examples/invalid/legacy-ensure.orna` | parse | `ORNA-A091-010` | `ORNA-A091-010` | substring match | MATCH |
| `examples/invalid/legacy-fact.orna` | parse | `ORNA-A091-010` | `ORNA-A091-010` | substring match | MATCH |
| `examples/invalid/legacy-failure-replay-method.orna` | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | substring match | MATCH |
| `examples/invalid/legacy-failure-resolve-method.orna` | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | substring match | MATCH |
| `examples/invalid/legacy-field-check.orna` | parse | `ORNA091-E-FIELD-CONSTRAINT` | `ORNA091-E-FIELD-CONSTRAINT` | substring match | MATCH |
| `examples/invalid/legacy-field-unique.orna` | parse | `ORNA091-E-FIELD-CONSTRAINT` | `ORNA091-E-FIELD-CONSTRAINT` | substring match | MATCH |
| `examples/invalid/legacy-ingest.orna` | parse | `E1004` | `E1004` | substring match | MATCH |
| `examples/invalid/legacy-log.orna` | parse | `E1001` | `E1001` | substring match | MATCH |
| `examples/invalid/legacy-match.orna` | parse | `ORNA091-E-MATCH` | `ORNA091-E-MATCH` | substring match | MATCH |
| `examples/invalid/legacy-opaque.orna` | parse | `ORNA091-E-OPAQUE` | `ORNA091-E-OPAQUE` | substring match | MATCH |
| `examples/invalid/legacy-pipe-lambda.orna` | parse | `E1011` | `E1011` | substring match | MATCH |
| `examples/invalid/legacy-postfix-question.orna` | parse | `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | substring match | MATCH |
| `examples/invalid/legacy-refined-where.orna` | parse | `ORNA-A091-001` | `ORNA-A091-001` | substring match | MATCH |
| `examples/invalid/legacy-result.orna` | typecheck | `ORNA091-E-RESULT` | `ORNA091-E-RESULT` | substring match | MATCH |
| `examples/invalid/legacy-return-arrow.orna` | parse | `ORNA091-E-RETURN-ARROW` | `ORNA091-E-RETURN-ARROW` | substring match | MATCH |
| `examples/invalid/legacy-store.orna` | parse | `E1003` | `E1003` | substring match | MATCH |
| `examples/invalid/legacy-stream-retry-method.orna` | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | substring match | MATCH |
| `examples/invalid/legacy-stream-skip-method.orna` | typecheck | `ORNA100-E-SYS-ADMIN-METHOD` | `ORNA100-E-SYS-ADMIN-METHOD` | substring match | MATCH |
| `examples/invalid/legacy-sys-runtime.orna` | resolve | `ORNA100-E-SYS-RUNTIME` | `ORNA100-E-SYS-RUNTIME` | substring match | MATCH |
| `examples/invalid/legacy-sys-storage-call.orna` | typecheck | `ORNA100-E-SYS-STORAGE-CALL` | `ORNA100-E-SYS-STORAGE-CALL` | substring match | MATCH |
| `examples/invalid/legacy-top-level-impl.orna` | parse | `ORNA091-E-IMPL-FOR` | `ORNA091-E-IMPL-FOR` | substring match | MATCH |
| `examples/invalid/legacy-tryfrom.orna` | resolve | `ORNA091-E-TRYFROM` | `ORNA091-E-TRYFROM` | substring match | MATCH |
| `examples/invalid/legacy-var.orna` | parse | `ORNA091-E-VAR` | `ORNA091-E-VAR` | substring match | MATCH |
| `examples/invalid/legacy-view.orna` | parse | `E1002` | `E1002` | substring match | MATCH |
| `examples/invalid/missing-required-field.orna` | typecheck | `E2002` | `E2002` | substring match | MATCH |
| `examples/invalid/module-single-table-assertion.orna` | typecheck | `ORNA-A091-003` | `ORNA-A091-003` | substring match | MATCH |
| `examples/invalid/module-zero-table-assertion.orna` | typecheck | `ORNA-A091-012` | `ORNA-A091-012` | substring match | MATCH |
| `examples/invalid/money-float.orna` | typecheck | `E5004` | `E5004` | substring match | MATCH |
| `examples/invalid/mutate-sys-commit.orna` | typecheck | `E7001` | `E7001` | substring match | MATCH |
| `examples/invalid/question-coalesce-adjacent.orna` | parse | `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | substring match | MATCH |
| `examples/invalid/question-on-int.orna` | parse | `ORNA091-E-POSTFIX-QUESTION` | `ORNA091-E-POSTFIX-QUESTION` | substring match | MATCH |
| `examples/invalid/range-key-overlap-magic.orna` | typecheck | `E3010` | `E3010` | substring match | MATCH |
| `examples/invalid/record-punning.orna` | parse | `E1013` | `E1013` | substring match | MATCH |
| `examples/invalid/rekey-auto-id.orna` | typecheck | `E3013` | `E3013` | substring match | MATCH |
| `examples/invalid/relation-equality.orna` | typecheck | `E4107` | `E4107` | substring match | MATCH |
| `examples/invalid/reserved-std.orna` | typecheck | `E1009` | `E1009` | substring match | MATCH |
| `examples/invalid/reserved-sys.orna` | typecheck | `E1008` | `E1008` | substring match | MATCH |
| `examples/invalid/row-declaration.orna` | parse | `E8001` | `E8001` | substring match | MATCH |
| `examples/invalid/secret-display.orna` | typecheck | `E7002` | `E7002` | substring match | MATCH |
| `examples/invalid/static-protocol-function.orna` | parse | `ORNA091-E-STATIC-FN` | `ORNA091-E-STATIC-FN` | substring match | MATCH |
| `examples/invalid/top-level-expression.orna` | parse | `E1006` | `E1006` | substring match | MATCH |
| `examples/invalid/top-level-on.orna` | parse | `E1005` | `E1005` | substring match | MATCH |
| `examples/invalid/transaction-block.orna` | parse | `E1007` | `E1007` | substring match | MATCH |
| `examples/invalid/two-durable-sources.orna` | typecheck | `E9102` | `ORNA-S021-TYPE` | substring match | MISMATCH: primary_diagnostic(emission) |
| `examples/invalid/unknown-field.orna` | typecheck | `E2003` | `E2003` | substring match | MATCH |
| `examples/invalid/unparenthesized-lambda-stage.orna` | parse | `E1204` | `E1204` | substring match | MATCH |
| `examples/invalid/unsafe-row-key-repeat.orna` | row-validation | `E3004` | `E3004` | substring match | MATCH |
| `examples/invalid/wrong-field-type.orna` | typecheck | `E2001` | `E2001` | substring match | MATCH |

Observed lower-level diagnostic payload for all 80 fixtures: severity `error`, spans `[]`. The adapter published the sidecar stable code and its message contained `message_contains` for 79 fixtures. The only remaining mismatch is the known `two-durable-sources.orna` stable-code mapping below.

## Exact mismatches and attribution

1. `examples/invalid/two-durable-sources.orna` — sidecar `primary_diagnostic` is `E9102`; emitted public diagnostic code is `ORNA-S021-TYPE` (native code also `ORNA-S021-TYPE`). The emitted message is `a durable consumer function may own only one checkpointed source root; extract separate named consumer functions`, so the sidecar `message_contains` value matches. Exact mismatching field: `primary_diagnostic`.
The remaining mismatch is an independently known mismatch guarded by the audit; this task retains it unchanged. The `legacy-result.orna` mismatch is resolved in `crates/orna-conformance-v1/src/semantic_adapter.rs`: when `ORNA091-E-RESULT` is present, it takes precedence over incidental unresolved-name diagnostics, matching the 1.1.0 requirement that unresolved Result/Ok/Err control plumbing emits that stable code.

The postfix diagnostic emitter and its fixture expectations now use the wording specified by Orna 1.1.0. No audit case was removed or weakened.

## Focused evidence

Command:

```text
unset ORNA_REFERENCE_DIR; export PATH="$HOME/.local/bin:$PATH" TMPDIR=/home/kieran/dev/ornadb/target/tmp BEADS_DIR=/home/kieran/dev/ornadb/work/.beads CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=/home/kieran/dev/ornadb/target/payload-audit; systemd-run --user --scope -p MemoryMax=4G -- cargo test --locked -p orna-conformance-v1 --test expected_diagnostics_payload_audit
```

Observed focused audit: the `legacy-result.orna` and `question-on-int.orna` payloads now match their sidecars; only the pre-existing `two-durable-sources.orna` stable-code mismatch remains under the audit’s guarded expected set. The test completed with exit code `0`.

The production diagnostic path now emits the spec-aligned postfix wording, semantic adapter selection prioritizes the specified Result diagnostic, and copied frozen-reference sidecars match the updated postfix message.
