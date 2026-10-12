# ORNA-CONF-002 linked-test execution record

Release epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)

Base: `origin/main` at `f919773d80f80dd8aafc73d8065fa40c9b1ffac3`

Frozen reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Requirement and register boundary

`source/01-scope.md:52` states **ORNA-CONF-002**: “A claim MUST distinguish supported optional profiles from mandatory facilities and list the implementation tests actually executed.” This increment records actual executions from the selected-test inventory previously linked to the CONF-002 evidence; it makes no product conformance-class or optional-profile support claim.

The frozen `tests/requirement-evidence.json` entry remains unchanged: `implementation_result` is `not executed`, the implementation-conformance obligation is `planned`, and `full_implementation_coverage_claimed` is `false`. These bounded runs do not satisfy that generic obligation and do not establish full conformance.

## Executed commands and outcomes

The complete captured stdout/stderr for every invocation, including compile warnings, failures, and retries, is in [the cargo transcript](orna-conf-002-cargo-test-transcripts-epic89-2026-09-28.log). Each transcript section records the exact command and captured exit code. It retains initial path-diagnosis runs and the intermediate-base outcomes, then records all six selected commands rerun on the base above. Those final runs use `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0` so tests resolve the frozen reference explicitly.

| Selected test command | Actual outcome | Exit |
| --- | --- | ---: |
| `cargo test --locked --offline -p orna-cli-v1 authoritative_reference_project_runs_seed_exercise_and_sensor_stream -- --nocapture` | Initial nested-checkout invocation could not compile `diagnostic_output.rs` because its `include_str!` path did not resolve. Final run on this base: selected test passed (1 passed; 64 filtered; remaining targets filtered). | 101, then 0 |
| `cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus` | Initial nested-checkout invocation: 3 passed, 41 failed because `tests/conformance-manifest.json` could not be read. Final run on this base: 44 passed, 0 failed. | 101, then 0 |
| `cargo test --locked --offline -p orna-traceability-v1` | At intermediate base `738b347`, 23 passed and `digest_bound_engine_witnesses_add_only_explicit_executed_boundaries` failed because the frozen requirement evidence did not declare `valid/minimal-root.orna`. After origin/main added the exact fixture declaration, final run on this base passed: 24 unit, 2 engine-witness, 1 integrated-evidence, and 2 status-boundary tests; doc tests 0. | 101 at 738b347; 0 at this base |
| `cargo test --locked --offline -p orna-traceability-v1 -- --skip digest_bound_engine_witnesses_add_only_explicit_executed_boundaries` | Initial invocations had worktree/reference-path failures, retained verbatim. Final run on this base: 23 unit passed with 1 filtered; engine-witness 2 passed; integrated-evidence 1 passed; status-boundary 2 passed; doc tests 0. | 101 on initial invocation; 0 at this base |
| `cargo test --locked --offline -p orna-repository-v1 runtime_owner_lock_excludes_a_second_owner_until_drop -- --nocapture` | Final run on this base failed before establishing its assertion: the Git commit proxy rejected temporary identity `owner-lock@example.invalid` as outside the identity allow-list. | 101 |
| `cargo test --locked --offline -p orna-runtime-v1 --lib stale_capture_and_competing_owner_are_distinct -- --nocapture` | Final run on this base: `stale_capture_and_competing_owner_are_distinct` passed (1 passed; 210 filtered). | 0 |

Failure details are preserved verbatim in the transcript, including `couldn't read ... wrong-field-type.orna`, `CorpusError("cannot read reference JSON: tests/conformance-manifest.json")`, the intermediate-base `TraceError("engine witness requirement evidence does not declare fixture: valid/minimal-root.orna")`, and the proxy's identity rejection. The exact fixture declaration change on the current base is why the later full traceability run passes; the earlier failure remains recorded. No observed failure was omitted or rewritten.

No product conformance claim is made. The frozen evidence register was not edited. No Rust or Orna source, fixture, or test file was changed; the selected existing tests use the repository's existing fixtures. The only changes in this increment are this report and its captured test transcript.
