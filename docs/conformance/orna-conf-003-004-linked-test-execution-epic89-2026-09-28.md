# ORNA-CONF-003/004 Linked-Test Execution Evidence

Release epic: `ornadb-1787968123319-16-24513f57` (GitHub #89), increment 11

Base: `origin/main` at `662876f829c6677faed70c2f489b7908344640eb`

Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`

`source/01-scope.md` SHA-256: `26b8cc52da70ed17a0efd3fe2ef1b0da4f55a64e689b68e952dab24a0786ef06`

`tests/requirement-evidence.json` SHA-256: `153c4c0f9678c52338dfd847e7aa706d888c9441d3b0924d013a6b7015d7a5aa`

## Scope and register boundary

This increment executes selected implementation tests linked from the repository's CONF-003 typecheck and CONF-004 extension evidence. It is one bounded execution record for this clause family; it is not a claim that every applicable conformance fixture or behavior test passed.

The frozen `tests/requirement-evidence.json` has a generic implementation-conformance obligation for each clause, not a concrete test command. Its current CONF-003 and CONF-004 entries remain `implementation_result: not executed`, `tests[0].status: planned`, and `full_implementation_coverage_claimed: false`. This partial evidence does not satisfy those full obligations. The frozen reference file was not modified.

## ORNA-CONF-003: semantic typecheck behavior

The selected test `semantic_adapter_keeps_type_errors_in_the_typecheck_phase` loads the checked-in `fixtures/semantic-validation/type-error-bad-table.orna` through `include_str!`, confirms semantic resolution succeeds, and observes rejection at typecheck with a type diagnostic.

Command:

```text
cargo test --locked --offline -p orna-conformance-v1 --test semantic_runtime_adapter semantic_adapter_keeps_type_errors_in_the_typecheck_phase -- --exact --nocapture
```

Captured result from `/tmp/ornadb-release-conf-evidence-11/conf-003-typecheck.log`:

```text
running 1 test
test semantic_adapter_keeps_type_errors_in_the_typecheck_phase ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out
EXIT_CODE=0
```

## ORNA-CONF-004: protocol extension behavior

The `orna-protocol-v1` package suite exercises optional extension fingerprint participation, preservation of ignorable result extensions, malformed-extension rejection, and protected-value rejection. These tests exercise protocol values directly and do not load Orna source.

Command:

```text
cargo test --locked --offline -p orna-protocol-v1
```

Captured result from `/tmp/ornadb-release-conf-evidence-11/conf-004-protocol.log`:

```text
test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

## Outcome

Both selected commands exited 0 with the outcomes above; neither captured run reported a test failure. These selected runs do not establish complete CONF-003 fixture/behavior coverage or all-profile CONF-004 extension invariance. No conformance class is claimed, and the frozen registry status remains unchanged.
