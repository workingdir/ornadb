# ORNA-CONF-001 Executed Evidence (Bounded)

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Repository base: `662876f829c6677faed70c2f489b7908344640eb` (`origin/main`)
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Clause and register boundary

`ORNA-CONF-001` (`source/01-scope.md:50`) says: “A conformance claim MUST
identify its classes, implementation version and exact specification
publication digest.” The frozen `tests/requirement-evidence.json` entry links
only a planned `implementation-conformance obligation`; it supplies no test
path or command. That reference register is unchanged. Its
`implementation_result: not executed` and `full_implementation_coverage_claimed:
false` remain authoritative; these selected traceability tests do not establish
an ORNA-CONF-001 implementation-conformance pass or make a conformance claim.

The commands below exercise existing traceability report and witness tests
related to publication identity and requirement evidence. They do not validate
the complete clause, including a claim's classes and implementation version.
Tests read the frozen corpus fixture `examples/valid/minimal-root.orna`; no
inline Orna program or fixture change was involved.

## Captured executions

Complete stdout/stderr from each invocation, including build warnings and
failure text, is preserved in the adjacent
`orna-conf-001-cargo-test-transcripts.log`; the relevant output and exit
statuses are reproduced below.

Full traceability package run:

```text
$ cargo test --locked --offline -p orna-traceability-v1
running 24 tests
test tests::digest_bound_engine_witnesses_add_only_explicit_executed_boundaries ... FAILED
...
---- tests::digest_bound_engine_witnesses_add_only_explicit_executed_boundaries stdout ----

thread 'tests::digest_bound_engine_witnesses_add_only_explicit_executed_boundaries' panicked at crates/orna-traceability-v1/src/lib.rs:1596:14:
digest-bound witness is accepted: TraceError("engine witness requirement evidence does not declare fixture: valid/minimal-root.orna")
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    tests::digest_bound_engine_witnesses_add_only_explicit_executed_boundaries

test result: FAILED. 23 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.07s

error: test failed, to rerun pass `-p orna-traceability-v1 --lib`
exit: 101
```

The first focused integration invocation omitted `ORNA_REFERENCE_DIR` and
failed before the test body because this worktree's default relative reference
path is unavailable:

```text
$ cargo test --locked --offline -p orna-traceability-v1 --test traceability_integrated_evidence -- --nocapture
running 1 test
thread 'declared_parse_witness_preserves_publication_identity_and_partial_requirement_boundary' panicked at crates/orna-traceability-v1/tests/traceability_integrated_evidence.rs:49:37:
read authoritative reference directory: Os { code: 2, kind: NotFound, message: "No such file or directory" }
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test declared_parse_witness_preserves_publication_identity_and_partial_requirement_boundary ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
error: test failed, to rerun pass `-p orna-traceability-v1 --test traceability_integrated_evidence`
exit: 101
```

Rerunning that integration target with the authoritative reference directory
set passed:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-traceability-v1 --test traceability_integrated_evidence -- --nocapture
running 1 test
test declared_parse_witness_preserves_publication_identity_and_partial_requirement_boundary ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.04s
exit: 0
```

The engine-witness binding target also passed against the explicit reference:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-traceability-v1 --test engine_witness_binding -- --nocapture
running 2 tests
test engine_witness_accepts_requirement_declared_for_exact_fixture_and_path ... ok
test engine_witness_rejects_unrelated_globally_known_requirement ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.03s
exit: 0
```

## Result

The focused report test preserved the frozen publication digest through a
partial requirement-evidence report, and the witness test rejected attaching
an unrelated `ORNA-CONF-001` requirement to a fixture witness. The full
package suite failed in that initial pre-PR-#1896 run, as captured above.
These bounded observations do not assert that any conformance claim identifies
all three required fields;
ORNA-CONF-001 therefore remains unexecuted in the frozen reference register.

## Post-witness-fixture repair rerun (2026-09-28)

After witness fixture repair PR #1896 (`ornadb-nfk5`) merged, the full package
suite was rerun against live `origin/main` at
`f919773d80f80dd8aafc73d8065fa40c9b1ffac3` with the authoritative reference
directory:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-traceability-v1
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CARGO_TEST_EXIT=0
```

The complete stdout/stderr, including build warnings, is appended to
`orna-conf-001-cargo-test-transcripts.log`. This rerun resolves the earlier
witness-binding failure; the prior failing output above is retained verbatim
as the pre-PR-#1896 result. No current package failures remain. This suite
continues to test traceability witness binding, not a complete conformance
claim; the frozen `ORNA-CONF-001` register remains `not executed` and
`full_implementation_coverage_claimed: false`.
