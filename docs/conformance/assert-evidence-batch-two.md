# ORNA-ASSERT evidence batch two

This batch adds ten executable witnesses for rows pinned as absent in
`docs/conformance/behavioral-scenario-batch-one.md` (scenarios 7 and 14).
Evidence labels follow ORNA-TEST-004 (`/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/32-conformance.md:23`):
**Specified** means a normative case exists, **Exists** means a test witness is
checked in, and **Passed** means the focused test target actually ran
successfully. Each row below has `Specified: yes; Exists: yes; Passed: yes`.

| Requirement | Verified source anchor | Executable witness | Evidence boundary |
|---|---|---|---|
| ORNA-ASSERT-004 | `source/07-tables.md:178` | `assert_004_checks_the_complete_unpublished_candidate_relation` | TransactionalEvaluator evaluates and commits the candidate after insert/update/re-key/delete operations; verifies final committed relation. |
| ORNA-ASSERT-005 | `source/07-tables.md:180` | `assert_005_includes_insert_update_delete_and_rekey_in_candidate_evidence` | Same fixture performs all four mutation forms and checks resulting rows. |
| ORNA-ASSERT-013 | `source/07-tables.md:196` | `assert_013_false_table_assertion_blocks_candidate_visibility` | False table predicate returns the table assertion diagnostic and exposes neither pending row. |
| ORNA-ASSERT-014 | `source/07-tables.md:198` | `assert_014_table_assertion_failure_rolls_back_all_candidate_writes` | Verifies both writes are absent after an assertion-aborted transaction. |
| ORNA-ASSERT-021 | `source/07-tables.md:237` | `assert_021_valid_module_assertion_resolves_two_distinct_tables` | Semantic analyzer accepts the frozen valid cross-table example and reports two dependencies. |
| ORNA-ASSERT-022 | `source/07-tables.md:239` | `assert_022_module_assertion_with_one_table_dependency_is_rejected` | Semantic analyzer reports `DIAG_ASSERTION_ONE_TABLE` for the frozen invalid example. |
| ORNA-ASSERT-023 | `source/07-tables.md:241` | `assert_023_module_assertion_without_table_dependencies_is_rejected` | Semantic analyzer reports `DIAG_ASSERTION_SCOPE` for the frozen invalid example. |
| ORNA-ASSERT-026 | `source/07-tables.md:247` | `assert_026_changed_dependency_tables_trigger_the_module_assertion` | A transaction changing dependency tables evaluates the module assertion; false result aborts and publishes neither candidate row. |
| ORNA-ASSERT-027 | `source/07-tables.md:249` | `assert_027_table_assertions_fail_before_module_assertions` | Simultaneously false table and module predicates; observed first diagnostic is table-owned, with both candidate writes rolled back. |
| ORNA-ASSERT-029 | `source/07-tables.md:253` | `assert_029_false_module_assertion_rolls_back_the_complete_database_candidate` | False module predicate aborts the transaction and leaves both tables without their pending rows. |

Runtime witnesses load `tests/fixtures/assert-evidence-batch-two/runtime-boundaries.orna`
with `include_str!`. They demonstrate the checked-in TransactionalEvaluator
boundary; they do not claim execution by an independent/full Orna engine,
storage engine, or every production commit/merge/checkout/publication path.
The three module-shape witnesses load the frozen reference's valid and invalid
cross-table assertion examples and establish semantic-analyzer acceptance or
diagnostic shape only. No row claims anything beyond its stated boundary.

## Pinned remainder

The following ASSERT requirements named by the two absent pinned scenarios are
not covered by this batch: ORNA-ASSERT-003, -015, -016, -017, -024, -025,
-028, and -030. Their source anchors are respectively `source/07-tables.md:176,
200,202,204,243,245,251,255`; scenario 15 also pins -055/-056 at lines 563
and 565, which remain uncovered.

Other ASSERT requirements in the pinned absent tail remain open by ID:

- checkout: ORNA-ASSERT-044, -049 (`source/07-tables.md:539,549`)
- commit: ORNA-ASSERT-041, -042 (`source/07-tables.md:533,535`)
- diagnostics: ORNA-ASSERT-051, -052, -053, -054, -060 (`source/07-tables.md:555,557,559,561,573`)
- merge: ORNA-ASSERT-043 (`source/07-tables.md:537`)
- publish: ORNA-ASSERT-046 (`source/07-tables.md:543`)
- refine: ORNA-ASSERT-033 and ORNA-REFINE-001/-002/-003 (`source/07-tables.md:515`; `source/05-types.md:399,401,403`)
- rewrite: ORNA-ASSERT-045, -050 (`source/07-tables.md:541,551`)
- system assertion: ORNA-ASSERT-059 (`source/07-tables.md:571`)
- table ownership: ORNA-ASSERT-003, -016, -017, -055, -056 (`source/07-tables.md:176,202,204,563,565`)

The frozen register is unchanged. Batch-one scenario rows and their
implementation-level status remain as recorded there; this document reports
only the ten witnesses and bounded run described above.
