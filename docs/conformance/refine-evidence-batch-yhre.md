# Refined type evidence batch (yhre)

This batch adds fixture-backed semantic-analyzer evidence for the three requirements left open by PR #2116, then records seven high-impact adjacent requirements. The fixtures are `crates/orna-semantic-v1/tests/fixtures/refine-evidence-yhre/ports.orna` and `owner_scope.orna`; the runner is `crates/orna-semantic-v1/tests/refine_evidence_batch_yhre.rs` and loads both with `include_str!`.

Evidence states follow **ORNA-TEST-004** (`source/32-conformance.md:23`): a requirement can be specified, have an implementation test, and pass that test independently. A passing semantic analysis does not establish runtime behavior.

## Requirement rows

| Requirement | Specified | Exists | Passed | Evidence and boundary |
|---|---|---|---|---|
| ORNA-REFINE-001 (`source/05-types.md:399`) | Yes | Yes: semantic analyzer fixture/test | Partial: the analyzer accepts two declaration assertions and emits two `RefinedType("Port")` plans. It does not execute them or establish runtime evaluation order. | `ports.orna`; analyzer declaration-plan boundary only. |
| ORNA-REFINE-002 (`source/05-types.md:401`) | Yes | No runtime conformance test | No | The fixture includes `Port.from(0)`, but analysis does not construct the value or emit an assertion failure. Safe presentation and source-linked runtime diagnostics remain untested. |
| ORNA-REFINE-003 (`source/05-types.md:403`) | Yes | Yes: semantic analyzer fixture/test | Yes, within analysis: subjectless `>= 1` and `<= 65_535` assertions are accepted and recorded under the refined owner. Candidate-value predicate evaluation is not run. | `ports.orna`; parse/name/type analysis and plan ownership only. |
| ORNA-NOMINAL-003 (`source/05-types.md:370`) | Yes | Yes: semantic analyzer fixture/test | Yes, within signatures: `default_port` returns `Port`, `raw_port` returns `Int`, and the types differ. Runtime representation is not inspected. | `ports.orna`; semantic signature identity only. |
| ORNA-REFINE-005 (`source/05-types.md:407`) | Yes | No build/runtime-mode conformance test | No | The analyzer test has no build-mode or runtime-mode matrix and cannot establish enforcement in every mode. |
| ORNA-IMPL-004 (`source/05-types.md:493`) | Yes | Yes: semantic analyzer fixture/test | Yes, within analysis: the refined `Port` declaration with nested `Display` implementation type-checks. Protocol dispatch at runtime is not exercised. | `ports.orna`; declaration acceptance only. |
| ORNA-ASSERT-034 (`source/07-tables.md:517`) | Yes | Yes: semantic analyzer fixture/test | Partial: `owner_scope.orna` keeps ordinary module-level `self` unresolved while the refined assertion receives a refined owner plan. Nested closures, exports, and stored values are not tested. | One ordinary module-level lookup only; not a complete non-leak proof. |
| ORNA-ASSERT-041 (`source/07-tables.md:533`) | Yes | No runtime construction/conversion test | No | The fixture names `Port.from` for valid and rejected candidates; analyzer acceptance does not prove assertions run at either conversion boundary. |
| ORNA-ASSERT-051 (`source/07-tables.md:555`) | Yes | No runtime failure-diagnostic test | No | No assertion executes, so no emitted failure span or owner is observed. |
| ORNA-ASSERT-052 (`source/07-tables.md:557`) | Yes | No runtime failure-presentation test | No | No failed candidate is presented; safe candidate rendering remains untested. |

The seven adjacent selections prioritize refined identity, always-on enforcement, owner-scope isolation, validation at conversion, failure provenance, and safe candidate presentation. ORNA-IMPL-004 is included because it defines a capability within the same refined declaration form. The static evidence is intentionally narrower than each full runtime contract.

## Captured focused proof

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-semantic-v1 --test refine_evidence_batch_yhre -- --nocapture
```

Captured output:

```text
    Blocking waiting for file lock on build directory
   Compiling orna-semantic-v1 v1.0.0 (/home/pbox/dev/ornadb/wt-refine-evidence-batch-yhre/crates/orna-semantic-v1)
    Finished `test` profile [unoptimized] target(s) in 40.35s
     Running tests/refine_evidence_batch_yhre.rs (/var/tmp/pbox-build/cargo/debug/deps/refine_evidence_batch_yhre-8b60beac004f8196)

running 2 tests
test implicit_refined_subject_does_not_become_an_ordinary_module_name ... ok
test refined_type_declarations_have_owner_plans_and_static_identity ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
FOCUSED_TEST_EXIT_CODE=0
```

The plan vector contains two same-shaped records, so this run does **not** prove their source order or runtime assertion order; no source-order pass is claimed here.

## Remaining related IDs pinned

These related clauses are outside the ten-row batch; no coverage claim is made for them here:

- `ORNA-REFINE-004` (`source/05-types.md:405`).
- `ORNA-IMPL-002`, `ORNA-IMPL-003`, `ORNA-IMPL-005`, `ORNA-IMPL-006` (`source/05-types.md:489,491,495,497`).
- `ORNA-ASSERT-033`, `ORNA-ASSERT-035`–`ORNA-ASSERT-040` (`source/07-tables.md:515,519,521,523,525,527,529`).
- `ORNA-ASSERT-049`, `ORNA-ASSERT-050` (`source/07-tables.md:549,551`).
- `ORNA-ASSERT-053`–`ORNA-ASSERT-060` (`source/07-tables.md:559,561,563,565,567,569,571,573`).
