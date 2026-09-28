# ORNA-TEST-001/003/004 Executed Evidence

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`
Tested repository HEAD: `8810fb8073399922f3e590cf3a244bd8bb2126e7`

The clause text was checked in `source/32-conformance.md` before execution.
The selected statements require valid fixtures to pass parse, resolution and
typecheck (ORNA-TEST-001); the complete reference project to include reachable
modules and all loose rows with path keys and schema validation
(ORNA-TEST-003); and evidence reporting to distinguish planned, implemented
and passed tests without claiming source execution from document checks
(ORNA-TEST-004).

The frozen `tests/requirement-evidence.json` entries for all three clauses
remain `implementation_result: not executed`, their test entries remain
`planned`, and `full_implementation_coverage_claimed` remains `false`. Its
SHA-256 is `153c4c0f9678c52338dfd847e7aa706d888c9441d3b0924d013a6b7015d7a5aa`;
it was not edited. The clause file SHA-256 was
`89fd8e2cd9986fe874d3917cbfad30dd060439d4fda87ed07a57ea2a65ecf2df`.

## Executed checks

### ORNA-TEST-001: selected valid source project

`semantic_adapter_typechecks_imported_generic_sys_meta_with_declared_metadata`
loads checked-in `.orna` module fixtures with `include_str!` and requires the
project parse, resolve and typecheck phases all to pass.

```text
$ cargo test --locked --offline -p orna-conformance-v1 --test semantic_runtime_adapter semantic_adapter_typechecks_imported_generic_sys_meta_with_declared_metadata -- --exact --nocapture
test semantic_adapter_typechecks_imported_generic_sys_meta_with_declared_metadata ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out
EXIT_CODE=0
```

This is one selected valid project, not execution of every valid corpus
fixture. Full ORNA-TEST-001 implementation coverage remains unestablished.

### ORNA-TEST-003: reference project and row admission

The CLI's existing reference-project integration target was attempted:

```text
$ cargo test --locked --offline -p orna-cli-v1 authoritative_reference_project_runs_seed_exercise_and_sensor_stream -- --exact --nocapture
error: couldn't read `crates/orna-cli-v1/tests/../../../../reference/Orna-1.0.0/examples/invalid/wrong-field-type.orna`: No such file or directory (os error 2)
 --> crates/orna-cli-v1/tests/diagnostic_output.rs:5:30
error: could not compile `orna-cli-v1` (test "diagnostic_output") due to 1 previous error
EXIT_CODE=101
```

The selected test could not compile because its existing `include_str!` path
does not resolve in this worktree. The CLI executable was then run against a
temporary Git-backed project populated by copying the real `.orna` modules
from `examples/reference/`:

```text
$ cargo run --locked --offline -p orna-cli-v1 -- init /var/tmp/ornadb-release-test-family/reference-project
EXIT_CODE=0
$ cargo run --locked --offline -p orna-cli-v1 -- --db /var/tmp/ornadb-release-test-family/reference-project check
project valid
EXIT_CODE=0
```

The frozen reference project contains five `.orna` modules and no loose row
files. Therefore this actual CLI check demonstrates the selected reference
modules were accepted by the project check, but does not establish the
ORNA-TEST-003 loose-row discovery, path-key, or schema-validation clauses. A
separate checked-in conformance test loads `.orna` inventory and row fixtures
with `include_str!` and exercises row admission and path-key reconstruction:

```text
$ cargo test --locked --offline -p orna-conformance-v1 --test semantic_runtime_adapter project_row_admission_resolves_declared_owner_path_key_and_evaluated_body -- --exact --nocapture
test project_row_admission_resolves_declared_owner_path_key_and_evaluated_body ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out
EXIT_CODE=0
```

That separate row fixture is not part of the reference project, so it does not
close the full-project requirement. No complete ORNA-TEST-003 pass is claimed.

### ORNA-TEST-004: status boundaries

The corpus test checks that the frozen requirement entries retain their
authoritative `not executed`, `planned`, and no-full-coverage markers. One
attempt without an explicit reference root failed before loading the corpus;
that environment/path failure is preserved here:

```text
thread 'requirement_evidence_keeps_all_authoritative_not_executed_markers' panicked at crates/orna-conformance-v1/tests/reference_corpus.rs:22:41:
reference corpus loads: CorpusError("cannot read reference JSON: tests/conformance-manifest.json")
test requirement_evidence_keeps_all_authoritative_not_executed_markers ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 43 filtered out
EXIT_CODE=101
```

Rerunning with the authoritative reference root set passed:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus requirement_evidence_keeps_all_authoritative_not_executed_markers -- --exact --nocapture
test requirement_evidence_keeps_all_authoritative_not_executed_markers ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 43 filtered out
EXIT_CODE=0
```

The traceability boundary test verifies that a non-engine implementation
evidence overlay does not create engine witnesses or executed requirement
statuses:

```text
$ cargo test --locked --offline -p orna-traceability-v1 tests::implementation_overlay_never_creates_engine_witnesses_or_executed_statuses -- --exact --nocapture
test tests::implementation_overlay_never_creates_engine_witnesses_or_executed_statuses ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 23 filtered out
EXIT_CODE=0
```

These checks exercise selected status safeguards. They do not change the
register's authoritative entries or establish full implementation coverage.

## Captured transcripts and limits

Full stdout/stderr from the commands above, including compiler warnings, is
stored locally at `/tmp/ornadb-release-test-family/` and
`/var/tmp/ornadb-release-test-family/`. The CLI test compilation failure is
preserved in `current-test-003-cli-target.log`; the actual CLI check is
recorded in `current-project-check.log`. The passing cargo test transcripts
from this tested HEAD are `current-test-001.log`, `current-test-003-row.log`,
`current-test-004-register.log`, and `current-test-004-overlay.log`. Earlier
attempts, including the default reference path failure and its successful
explicit-root rerun, remain in the same log directory.

This record reports only the selected executions and their limits. It does
not update or supersede the frozen evidence register, and the frozen register
continues to state that implementation evidence for these clauses is not
executed.
