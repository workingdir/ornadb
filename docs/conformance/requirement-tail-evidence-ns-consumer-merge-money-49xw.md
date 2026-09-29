# Requirement tail evidence batch: namespaces, consumers, merge, and Money

This record follows **ORNA-TEST-004** (`source/32-conformance.md:23`):
`Specified` is the frozen clause, `Exists` is the checked-in test witness, and
`Passed` is only the result and boundary shown below. All source locations are
in the frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0` tree.

The batch selects ten machine-testable requirements from the issue's four
families. Tests are new integration-test files and use checked-in `.orna`
fixtures with `include_str!`. The tests exercise the project loader, semantic
analyzer, or checkpoint-position helper as named; none runs a complete Orna
compiler/runtime project. These outcomes do not update the frozen
`tests/requirement-evidence.json` register, and do not constitute a full
conformance claim.

## Selected rows

| Requirement | Specified | Exists | Passed |
|---|---|---|---|
| **ORNA-NS-001** — `source/03-source-modules.md:22` | A directory's `main.orna` defines its namespace. | `requirement_tail_ns_49xw::root_module_file_module_and_directory_main_define_their_namespaces`, using `root-imports.orna` and `warehouse-main.orna`. | Yes: loader assigned `warehouse/main.orna` namespace `warehouse`. This does not test semantic resolution inside that directory. |
| **ORNA-NS-002** — `source/03-source-modules.md:24` | A non-`main.orna` filename adds its stem as final namespace component. | Same test, using `report.orna`. | Yes: loader assigned `report.orna` namespace `report`. |
| **ORNA-NS-004** — `source/03-source-modules.md:28` | `x.orna` and `x/main.orna` cannot own the same namespace. | `requirement_tail_ns_49xw::rejects_file_and_directory_main_that_own_the_same_namespace`. | Yes: loader returned `DuplicateModuleNamespace` for `reports.orna` and `reports/main.orna`. |
| **ORNA-NS-005** — `source/03-source-modules.md:32` | `sys` and `std` are the reserved top-level namespaces. | `requirement_tail_ns_49xw::rejects_source_owned_reserved_namespaces`. | Yes: source-owned `sys.orna` and `std.orna` were rejected, while `custom.orna` loaded. This is a sample path check, not an exhaustive proof over every possible namespace. |
| **ORNA-NS-008** — `source/03-source-modules.md:38` | Path components are NFC and siblings are unique under Unicode 16.0.0 toNFKC_Casefold. | `requirement_tail_ns_49xw::rejects_unicode_16_casefold_colliding_sibling_paths`. | Yes: project load rejected the fixture's U+10D50/U+10D70 sibling pair as `SiblingCollision`. NFC checks and rename/merge operations are outside this test. |
| **ORNA-CONSUMER-005** — `source/11-streams.md:40` | A durable consumer with multiple checkpointed source roots receives extraction guidance. | `requirement_tail_consumer_merge_money_49xw::durable_consumer_with_multiple_checkpointed_roots_gets_guidance`, using `streams-pa0p-multiple-roots.orna`. | Yes: semantic analysis emitted the diagnostic with the separate-named-consumer guidance. This is semantic analyzer evidence, not runtime execution. |
| **ORNA-CONSUMER-008** — `source/11-streams.md:56` | Equal checkpoints merge; divergent opaque positions must not be ordered by token value. | `requirement_tail_consumer_merge_money_49xw::equal_opaque_checkpoints_merge_and_divergent_positions_conflict`. | Yes: equal positions merged and distinct changed tokens produced a conflict. This tests the pure checkpoint-position helper, not synchronization across clones. |
| **ORNA-MERGE-011** — `source/25-evolution.md:97` | Divergent opaque checkpoints produce `sys.CheckpointConflict`, not a guessed merge. | Same checkpoint-position helper test. | Yes, bounded to preserving base/left/right in a conflict result. The test does not exercise database merge integration or the `sys.CheckpointConflict` diagnostic projection. |
| **ORNA-MONEY-001** — `source/05-types.md:276` | Money uses exact decimal semantics. | `requirement_tail_consumer_merge_money_49xw::money_literal_is_typed_exactly_and_cross_currency_addition_is_rejected`, using `traceability-money-exact-decimal.orna`. | Yes: semantic analysis accepted `12.34.GBP` with result type `Money<GBP>`. This does not establish runtime arithmetic, persistence, or codec exactness. |
| **ORNA-MONEY-002** — `source/05-types.md:278` | Different currency parameters cannot be added without explicit data-backed conversion. | Same test, using `traceability-money-cross-currency-add.orna`. | Yes: semantic analysis rejected unconverted `Money<GBP> + Money<USD>`. Conversion behavior is not exercised. |

## Executed proof

The exact focused commands and complete terminal output are captured outside
the repository under `/tmp/ornadb-49xw-logs/`:

- `cargo test --locked --offline -p orna-project-v1 --test requirement_tail_ns_49xw` — **exit 0**, `4 passed; 0 failed`. Final output: `orna-project-ns-rerun.log`; exit record: `orna-project-ns-rerun.exit`.
- `cargo test --locked --offline -p orna-conformance-v1 --test requirement_tail_consumer_merge_money_49xw` — **exit 0**, `3 passed; 0 failed`. Final output: `orna-conformance-tail-rerun.log`; exit record: `orna-conformance-tail-rerun.exit`.

The first conformance run exposed a test expectation mismatch (`expected
main.GBP`, inferred type was `GBP`) and exited 101. The test was corrected to
assert the observed nominal type; the original output is retained in
`orna-conformance-tail.log` with `orna-conformance-tail.exit`, and the passing
rerun is captured separately. No implementation behavior was changed.

## Pinned remainder

These in-scope IDs are not covered by this batch and remain unclaimed:

- **NS:** ORNA-NS-003, ORNA-NS-006, ORNA-NS-007.
- **CONSUMER:** ORNA-CONSUMER-001, ORNA-CONSUMER-002, ORNA-CONSUMER-003, ORNA-CONSUMER-004, ORNA-CONSUMER-006, ORNA-CONSUMER-007, ORNA-CONSUMER-009.
- **MERGE:** ORNA-MERGE-001, ORNA-MERGE-002, ORNA-MERGE-003, ORNA-MERGE-004, ORNA-MERGE-005, ORNA-MERGE-006, ORNA-MERGE-007, ORNA-MERGE-008, ORNA-MERGE-009, ORNA-MERGE-010.
- **MONEY:** ORNA-MONEY-003, ORNA-MONEY-004, ORNA-MONEY-005, ORNA-MONEY-006, ORNA-MONEY-007, ORNA-MONEY-008, ORNA-MONEY-009, ORNA-MONEY-010.

All 38 rows for these four families in the frozen requirement register remain
`planned` / `not executed`; this batch neither edits that register nor promotes
its own bounded test results into the frozen register.
