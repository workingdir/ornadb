# ORNA-ASSERT evidence batch three

This batch follows the candidate and cross-table remainder explicitly pinned by
PR #2116. Evidence labels use ORNA-TEST-004 (`source/32-conformance.md:23`):
`Specified` is normative scope, `Exists` is a checked-in executable witness,
and `Passed` means the focused test actually ran successfully. Partial
witnesses are marked partial and do not convert an unobserved clause portion
into a pass.

| Clause | Verified reference anchor | Specified | Exists | Passed | Witness and boundary |
|---|---|---:|---:|---:|---|
| ORNA-ASSERT-003 | `source/07-tables.md:176` | yes | yes | yes | Semantic analysis of the frozen table-assertion example emits plans owned by `User`; bounded to semantic owner metadata. |
| ORNA-ASSERT-015 | `source/07-tables.md:200` | yes | yes | yes | `coherent-candidate.orna` inserts related rows in both tables in one evaluator activation; table/module validation accepts and both rows commit. Bounded to TransactionalEvaluator's candidate boundary. |
| ORNA-ASSERT-016 | `source/07-tables.md:202` | yes | yes | yes | Frozen `legacy-assert-self-pipe.orna` reaches `ORNA-A091-002` and diagnostic guidance recommends removing `self |`. |
| ORNA-ASSERT-017 | `source/07-tables.md:204` | yes | yes | yes | Frozen `legacy-assert-owner-pipe.orna` reaches `ORNA-A091-002` and diagnostic guidance recommends removing the repeated table owner. |
| ORNA-ASSERT-024 | `source/07-tables.md:243` | yes | yes | yes | Valid cross-table module examples analyze without diagnostics, produce module-owned plans and resolve two table dependencies; bounded to semantic analysis. |
| ORNA-ASSERT-025 | `source/07-tables.md:245` | yes | yes | yes | Fixture's module-owned predicate with a network call emits `DIAG_ASSERTION_EFFECT`; bounded to semantic effect rejection, not execution. |
| ORNA-ASSERT-028 | `source/07-tables.md:251` | yes | partial | partial | Two module assertions with distinguishable dependencies remain in source-span order in one module's semantic plans. Stable owning-ObjectId ordering across modules is not exposed or proved by this adapter test. |
| ORNA-ASSERT-030 | `source/07-tables.md:255` | yes | yes | yes | Valid cross-table assertion has `AssertionOwner::Module` and two dependencies, rather than a table owner; bounded to semantic ownership. |
| ORNA-ASSERT-055 | `source/07-tables.md:563` | yes | partial | partial | Recognized `self |` legacy form has diagnostic guidance for migration. This does not establish a machine-applicable fix object. |
| ORNA-ASSERT-056 | `source/07-tables.md:565` | yes | partial | partial | An ordinary relation pipeline in a function remains accepted. No fixer API is invoked; non-rewriting of arbitrary `self`/pipeline forms by a fixer is not claimed. |

All Orna inputs in the new test target are loaded from checked-in `.orna`
fixtures or frozen-reference examples with `include_str!`. Runtime evidence
uses `TransactionalEvaluator`; the other cases use semantic analysis. These
are bounded implementation witnesses, not proof of a full independent Orna
engine or every storage/repository transaction path. No register is rewritten.

## Deeper pinned tail

The following IDs remain outside this batch as pinned for follow-up by the
accepted issue: ORNA-ASSERT-033, -041, -042, -043, -044, -045, -046, -049,
-050, -051, -052, -053, -054, -059, and -060. Their verified anchors are
`source/07-tables.md:515,533,535,537,539,541,543,549,551,555,557,559,561,571,573`.
ORNA-ASSERT-028's cross-module stable ObjectId order and the machine-fix /
conservative-fixer portions of -055/-056 also remain unproved, as noted above.
