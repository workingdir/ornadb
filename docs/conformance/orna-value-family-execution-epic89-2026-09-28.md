# ORNA-VALUE family execution record

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Test base: `origin/main` at `1e1c532fc6f2533a82695e4005604834dae49f41`
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Verified normative boundary

The frozen reference defines seven requirements in `source/05-types.md`:

- **ORNA-VALUE-001** (`:65`): primitive, list, record, enum, option, quantity, money, nominal and row values have value semantics.
- **ORNA-VALUE-002** (`:67`): queried rows are immutable snapshots including table identity, primary key, snapshot context and logical fields.
- **ORNA-VALUE-003** (`:69`): closures capture values at creation; table handles retain database/snapshot context unless changed explicitly.
- **ORNA-VALUE-004** (`:71`): ordinary list/record/nominal cycles cannot be constructed; persistent table-reference cycles may exist.
- **ORNA-VALUE-005** (`:73`): memory management, pointer identity and finalization timing are not observable semantics.
- **ORNA-VALUE-006** (`:75`): reassigning a `let` binding does not mutate previously captured, returned, stored or observed values.
- **ORNA-VALUE-007** (`:77`): prohibited `var` declaration contexts produce `ORNA091-E-VAR` and propose `let`.

The neighboring chapters `source/04-lexical.md` and `source/06-expressions.md` contain no ORNA-VALUE clauses. The frozen evidence records at `tests/requirement-evidence.json:905-1003` give each of the seven IDs only a generic planned implementation-conformance obligation, `implementation_result: "not executed"`, and `full_implementation_coverage_claimed: false`; no exact Cargo command is linked there. The records were read but not edited.

## Selected execution evidence

| Clause(s) | Existing check and result | Evidence boundary |
| --- | --- | --- |
| ORNA-VALUE-001, 003, 006 | `orna-evaluator-v1` test `closures_capture_immutable_snapshots_and_support_nested_calls`: 1 passed, 0 failed. It loads `crates/orna-evaluator-v1/tests/fixtures/closures.orna` via `include_str!` and checks scalar, record and list captures after reassignment, nested captures, and rejected capture mutation. | Selected evaluator evidence for those values and closures only. It does not cover every value category or table-handle context retention. |
| ORNA-VALUE-002 | `orna-table-v1` unit test `tests::table_snapshot_remains_stable_after_later_commit`: 1 passed, 0 failed. | Low-level table snapshot stability with generic rows; it does not establish the full Orna query-row identity/context contract. The first attempted exact filter selected 0 tests (exit 0); that output is retained and not counted as a pass. |
| ORNA-VALUE-006, 007 | `orna-conformance-v1` test `let_rebinding_executes_exact_runtime_checks_and_migration_diagnostic`: 1 passed, 0 failed. This is `BoundedEvaluator` scenario evidence; it checks replacement vs captured scalar/record/list values and reads its `var` diagnostic fixture. | Bounded evaluator only; not a compiler-produced Orna-engine witness. |
| ORNA-VALUE-006, 007 | `orna-conformance-v1` test `harness_distinguishes_executed_rebinding_from_unimplemented_scenarios`: 1 passed, 0 failed. The checked harness report distinguishes its four bounded scenarios from 140 skipped scenarios. | Confirms the bounded adapter boundary; does not promote this to full implementation conformance. |
| ORNA-VALUE-007 | `orna-syntax-v1` test `legacy_var_declarations_remain_rejected_at_statement_boundaries`: 1 passed, 0 failed. It loads real fixtures `v1_review_source_015.orna` and `v1_review_source_016.orna` with `include_str!` and checks `ORNA091-E-VAR`. | Parser diagnostic evidence; the separate bounded scenario check above verifies the `let` suggestion. |
| ORNA-VALUE-001..007 | `orna-conformance-v1` test `requirement_evidence_keeps_all_authoritative_not_executed_markers`: 1 passed, 0 failed. | Guard evidence confirms the frozen 870-entry register remains `not executed` / `planned` and no harness pass is inferred from the register. |

No direct test linked by the frozen register was found for ordinary-value cycle rejection or memory/pointer/finalization non-observability (ORNA-VALUE-004/005). Those clauses remain unverified here. ORNA-VALUE-001/002/003 also remain only partially covered by the selected checks described above. No clause is claimed fully passed.

The exact commands, complete captured stdout/stderr, and shell-captured exit codes are in [`orna-value-family-cargo-transcripts-epic89-2026-09-28.log`](orna-value-family-cargo-transcripts-epic89-2026-09-28.log). All six invocations that selected a test exited 0 with 1 passed and 0 failed. The retained initial table filter also exited 0 but ran 0 tests. No Rust source, test, Orna fixture, or frozen reference file changed.
