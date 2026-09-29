# Standard library conformance audit (ey09)

Audit base: `origin/main` at `b795700089ede4cbecfc109927982f2af89716f8`.
Reference: frozen Orna 1.0.0.

## Disposition

**Not conformant / not fully evidenced at this tip.** This audit found one
portable operation named by the reference with no implementation or test:
`asof_join`. The frozen clause states its selection and tie rules but does not
specify its result shape or the callable signatures/meaning of `time` and
`by` (`source/09-standard-library.md:107`). Implementing a result without that
interface would invent behavior, so this audit records a reference gap and
makes no production change for it.

The source-backed standard profile also contains only `std/math.orna`:
`orna-standard` embeds that one source and returns a one-element source array
(`crates/orna-standard/src/lib.rs:83-100`). The profile test verifies its
snapshot and a consumer import (`crates/orna-standard/src/tests/v1_profile.rs:8-26`).
There is no repository `stdlib/std/` directory. Existing Rust evaluator code
implements and tests many portable operations, but this is not a complete
source-backed inventory of the standard modules.

## Clause evidence

The IDs below are verified in the frozen reference's `source/09-standard-library.md`.

- **ORNA-LIB-001** (`:11`): the exact `std/math.orna` bytes and profile snapshot
  are checked; historical dependency and time-zone substitution behavior is
  not established by that focused test.
- **ORNA-LIB-002** (`:13`): the CLI rejects an uncaptured standard import in
  `crates/orna-cli-v1/tests/project_runtime.rs:461-481`; this is targeted
  import-boundary evidence, not a full core-without-std conformance run.
- **ORNA-LIB-003** (`:15`): partial operation evidence exists. For example,
  `pairs` is implemented at `crates/orna-evaluator-v1/src/lib.rs:3148-3150`
  and has six passing focused tests. No implementation or test match exists
  for `asof_join`; its normative reference entry is at
  `source/09-standard-library.md:107`.
- **ORNA-LIB-004** (`:44`): semantic tests cover effect checking for relational
  callbacks (`crates/orna-semantic-v1/tests/relation_bounds.rs:86-179`), but
  this audit did not establish coverage for every query plan and optimizer path.
- **ORNA-LIB-005** (`:46`): evaluator tests cover callback failure propagation
  for operations including `filter`, `partition`, `one`, `sort_by`, and
  `split_when`; they do not prove all collection operators.
- **ORNA-LIB-006** (`:77`): transactional source tests exercise table
  mutations and rollback, but this audit did not establish a complete
  operation-by-operation match for return values, missing keys, and redaction.
- **ORNA-LIB-007** (`:89`): real `.orna` fixtures exercise `Stream.from_list`,
  but this audit did not establish the full typed identity, replay, and
  preservation contract.

The frozen reference's `tests/requirement-evidence.json` marks ORNA-LIB-001
through ORNA-LIB-007 as `not executed` with planned implementation obligations
(entries around lines 6365-6463). ORNA-TEST-010 (`source/32-conformance.md:31`)
states that the evidence mapping is a test plan, not execution proof;
ORNA-TEST-011 (`:33`) requires executable evidence for machine-testable
behavior. Therefore the corpus plan cannot turn the partial implementation
tests above into a full conformance pass.

## Captured focused tests

At the audit base, both commands exited 0:

```text
$ cargo test --locked --offline -p orna-standard
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0

$ cargo test --locked --offline -p orna-evaluator-v1 pairs_relation
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

Full captured output: `/tmp/ornadb-stdlib-ey09-standard.log` and
`/tmp/ornadb-stdlib-ey09-pairs.log`.
