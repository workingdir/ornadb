# Behavioural scenario batch one

This report covers only scenarios 1–30 from the frozen
`tests/scenarios.json`. It follows **ORNA-TEST-004**: `specified` means the
reference defines the case, `exists` means this implementation has an
executable witness, and `passed` means that witness ran successfully. Each
scenario in the corpus is labelled “implementation scenario, not executed by
an Orna engine”; a pass below is therefore bounded implementation evidence,
not proof of full Orna source execution.

| # | Scenario | Disposition | Verified normative clauses and locations |
|---:|---|---|---|
| 1 | `ACTIVATION-001` | absent | ORNA-TXN-005, `source/10-execution.md:17`; ORNA-TXN-006, `:19` |
| 2 | `AFFINE-AGG-001` | absent | ORNA-UNIT-005, `source/05-types.md:223`; ORNA-UNIT-007, `:242` |
| 3 | `ARROW-001` | absent | ORNA-ARROW-001/002/003, `source/04-lexical.md:89,95,97` |
| 4 | `ASSERT-CHECKOUT-091` | absent | ORNA-ASSERT-044/049, `source/07-tables.md:539,549` |
| 5 | `ASSERT-CHECKPOINT-091` | passed | ORNA-ASSERT-047, `source/07-tables.md:545`; ORNA-CP-003, `source/12-checkpoints.md:11` |
| 6 | `ASSERT-COMMIT-091` | absent | ORNA-ASSERT-041/042, `source/07-tables.md:533,535` |
| 7 | `ASSERT-CROSSTABLE-091` | absent | ORNA-ASSERT-021/022/023/024/030, `source/07-tables.md:237,239,241,243,255` |
| 8 | `ASSERT-DIAGNOSTIC-091` | absent | ORNA-ASSERT-051/052/053/054/060, `source/07-tables.md:555,557,559,561,573` |
| 9 | `ASSERT-MERGE-091` | absent | ORNA-ASSERT-043, `source/07-tables.md:537` |
| 10 | `ASSERT-PUBLISH-091` | absent | ORNA-ASSERT-046, `source/07-tables.md:543`; ORNA-PUB-006, `source/22-publication.md:51` |
| 11 | `ASSERT-REFINE-091` | absent | ORNA-REFINE-001/002/003, `source/05-types.md:399,401,403`; ORNA-ASSERT-033, `source/07-tables.md:515` |
| 12 | `ASSERT-REWRITE-091` | absent | ORNA-ASSERT-045/050, `source/07-tables.md:541,551`; ORNA-STORAGE-004, `source/23-storage.md:15` |
| 13 | `ASSERT-SYS-091` | absent | ORNA-ASSERT-059, `source/07-tables.md:571`; ORNA-INFER-005, `source/05-types.md:319`; ORNA-FN-010, `source/06-expressions.md:84` |
| 14 | `ASSERT-TABLE-CANDIDATE-091` | absent | ORNA-ASSERT-004/005/013/014/015, `source/07-tables.md:178,180,196,198,200` |
| 15 | `ASSERT-TABLE-OWNER-091` | absent | ORNA-ASSERT-003/016/017/055/056, `source/07-tables.md:176,202,204,563,565` |
| 16 | `AUTOID-REFS-001` | absent | ORNA-REMOTE-003/004/005, `source/24-git-history.md:48,50,52` |
| 17 | `CALL-001` | absent | ORNA-CALL-001/002, `source/08-relations.md:101,103`; ORNA-TXN-005, `source/10-execution.md:17` |
| 18 | `CANCELLATION-091` | absent | ORNA-ERR-004/005, `source/06-expressions.md:399,401`; ORNA-CANCEL-001, `source/10-execution.md:99` |
| 19 | `CASE-091` | absent | ORNA-CASE-001/002/003/004/005/006, `source/06-expressions.md:117,119,121,123,125,127` |
| 20 | `CFLOW-001` | absent | ORNA-CFLOW-001/002/003, `source/06-expressions.md:209,211,213` |
| 21 | `COMPACT-GENERATION-001` | absent | ORNA-COMPACT-014/015/019, `source/23-storage.md:47,49,57` |
| 22 | `COMPACT-MERGE-GENERATION-001` | absent | ORNA-COMPACT-016, `source/23-storage.md:51` |
| 23 | `COMPACT-SCHEMA-PROJECTION-001` | absent | ORNA-COMPACT-018, `source/23-storage.md:55`; ORNA-SCHEMA-003/004/007, `source/25-evolution.md:45,47,53` |
| 24 | `CONCUR-001` | absent | ORNA-CONCUR-001/002/003, `source/10-execution.md:42,85,87` |
| 25 | `CONSUMER-CROSSCLONE-001` | absent | ORNA-CONSUMER-007/009, `source/11-streams.md:54,58` |
| 26 | `CONVERT-FAILURE-091` | absent | ORNA-CONVERT-004/005/010, `source/05-types.md:438,440,450` |
| 27 | `CONVERT-MULTISOURCE-091` | absent | ORNA-CONVERT-001/002/003, `source/05-types.md:432,434,436`; ORNA-GENERIC-013/014, `source/06-expressions.md:185,187` |
| 28 | `CONVERT-NOCHAIN-091` | absent | ORNA-CONVERT-006/007/008, `source/05-types.md:442,444,446` |
| 29 | `CP-001` | passed | ORNA-CP-001, `source/12-checkpoints.md:7`; ORNA-CP-006, `:32` |
| 30 | `CP-002` | absent | ORNA-CP-006, `source/12-checkpoints.md:32` |

Counts: **30 specified, 2 with executable witnesses that passed, 28 absent, 0
failed** in the final run. The former `CP-001` failure exposed that the runtime
administration result mapper rejected a valid `Running` state after resume;
the mapper now preserves that outcome, with a focused public API test for
resume after reset and the idempotent running case. `CP-001` is exercised by
the bounded runtime-adapter scenario witness using the real
`scenario-cp-001.orna` fixture. `ASSERT-CHECKPOINT-091` likewise remains
runtime-adapter evidence. Neither pass claims compiler-produced Orna-engine
execution or the public `sys.Checkpoint` projection.

The remaining 114 scenarios are not covered by this batch and are listed in
the pull request body as explicit follow-up; none is represented as passed.
