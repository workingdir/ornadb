# ORNA-CLOSURE / ORNA-EMBED linked-test execution

Progress record for epic `ornadb-1787968123319-16-24513f57` (GitHub #89). This report records actual bounded test executions on `origin/main` at `8810fb8073399922f3e590cf3a244bd8bb2126e7`. It does not edit the frozen reference or `tests/requirement-evidence.json`, and it makes no full conformance claim.

## Verified frozen clauses

| Requirement | Frozen reference clause |
| --- | --- |
| ORNA-CLOSURE-002 | `source/32-conformance.md:76`: evidence obligations do not authorize different observable semantics. |
| ORNA-CLOSURE-003 | `source/32-conformance.md:78`: out-of-profile features are absent and must yield unsupported-feature or resolution diagnostics rather than vendor-specific behavior. |
| ORNA-CLOSURE-004 | `source/01-scope.md:62`: configuration cannot change specified language, identity, transaction, checkpoint, or repository semantics. |
| ORNA-EMBED-001 | `source/19-repository.md:104`: a clone remains usable without `orna serve` or an external Orna daemon. |
| ORNA-EMBED-002 | `source/19-repository.md:106`: local commands can open embedded Turso state in-process when no local owner exists. |
| ORNA-EMBED-003 | `source/19-repository.md:108`: overlapping writable handles may coordinate locally without requiring a network server. |
| ORNA-EMBED-004 | `source/19-repository.md:110`: correctness cannot depend on experimental multi-process Turso file access. |

Cross-check: `source/32-conformance.md:23` (ORNA-TEST-004) separates specified, implemented, and passed tests; `:69` (ORNA-EVIDENCE-001) prohibits inferring implementation evidence from association or inventory. The frozen `tests/requirement-evidence.json` entries for all seven clauses remain `implementation_result: not executed` with one planned implementation-conformance obligation and no executable command or result linked. No status was promoted here.

## Executed linked evidence

Commands use the existing route map in `docs/conformance/embedded-integration-evidence-orchestration.md`. Orna project input is the checked-in reference project and its real `.orna` files; this change adds no Orna source or fixture. Full captured stdout/stderr and exit markers for each attempt, including setup failures, are in `orna-closure-embed-test-transcripts-epic89-2026-09-28.log`.

| Command | Captured outcome | Exit |
| --- | --- | ---: |
| `cargo test --locked --offline -p orna-cli-v1 authoritative_reference_project_runs_seed_exercise_and_sensor_stream -- --nocapture` | `test tests::authoritative_reference_project_runs_seed_exercise_and_sensor_stream ... ok`; `1 passed; 0 failed; 64 filtered out`. The other test targets were filtered. | 0 |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus` | `44 passed; 0 failed`. This verifies the selected corpus/index and evidence-boundary checks; it is not execution of the closure clauses themselves. | 0 |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-traceability-v1` | Library `24 passed`; integration targets `2 + 1 + 2 passed`; binary/doc targets had zero tests. All targets passed. | 0 |
| `cargo test --locked --offline -p orna-repository-v1 runtime_owner_lock_excludes_a_second_owner_until_drop -- --nocapture` | `0 passed; 1 failed; 71 filtered out`; test setup was blocked before the assertion by the Git identity proxy (verbatim below). This is no owner-lock conformance result. | 101 |
| `cargo test --locked --offline -p orna-runtime-v1 --lib stale_capture_and_competing_owner_are_distinct -- --nocapture` | `test tests::stale_capture_and_competing_owner_are_distinct ... ok`; `1 passed; 0 failed; 210 filtered out`. This bounded unit test does not establish multi-process correctness. | 0 |

Verbatim owner-lock failure:

```text
git ["config", "user.email", "owner-lock@example.invalid"]: [git-commit-proxy] blocked: user.email value 'owner-lock@example.invalid' is not in the identity allow-list
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 71 filtered out; finished in 0.21s
```

## Preserved setup failures

The first CLI attempt ran from a nested Herdr worktree; its existing compile-time fixture path did not resolve at that checkout depth. No tests ran (exit 101):

```text
error: couldn't read `crates/orna-cli-v1/tests/../../../../reference/Orna-1.0.0/examples/invalid/wrong-field-type.orna`: No such file or directory (os error 2)
 --> crates/orna-cli-v1/tests/diagnostic_output.rs:5:30
help: there is a file with the same name in a different directory
5 | const INVALID_SOURCE: &str = include_str!("../../../../../reference/Orna-1.0.0/examples/invalid/wrong-field-type.orna");
error: could not compile `orna-cli-v1` (test "diagnostic_output") due to 1 previous error
```

The first reference-corpus attempt omitted `ORNA_REFERENCE_DIR`; 41 tests failed and 3 passed because `Corpus::load_default()` could not open the reference manifest. The exact repeated panic was:

```text
reference corpus loads: CorpusError("cannot read reference JSON: tests/conformance-manifest.json")
test result: FAILED. 3 passed; 41 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
```

The command was rerun with the authoritative reference path explicitly set and passed 44/44. Both setup failures remain reported; neither is treated as a product result.

## Evidence boundary

The selected CLI path provides bounded evidence for ORNA-EMBED-001/002 only. The attempted repository owner-lock test did not reach its assertion. The passing runtime unit test is bounded to its named case. Therefore ORNA-EMBED-003/004 remain unverified at the overlapping-process correctness boundary. The linked frozen register entries contain no concrete test command for ORNA-CLOSURE-002/003/004; the corpus and traceability suites passed as evidence-processing tests but do not establish those semantics. Those closure obligations remain not executed. No full runtime or profile conformance claim is made.
