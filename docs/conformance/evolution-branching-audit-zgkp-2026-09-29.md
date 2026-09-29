# Evolution and branching conformance audit

Issue: `ornadb-zgkp` / GitHub #2047  
Audit base: `origin/main` at `c5077cb79895864e23755bc66f7b09212d723adb`  
Authority: frozen Orna 1.0.0 reference, `source/25-evolution.md` and `source/21-branching.md`.

## Evidence grades

- **A — direct test:** an existing executable test directly exercises the criterion.
- **B — bounded test/code:** implementation and a narrower test exist, but the complete criterion is not established.
- **C — code inspection:** relevant code exists, but no direct conformance test was found in the audited paths.
- **G — implementation gap:** a repository-wide targeted source/test search found no matching production path or conformance test. This is a gap, not a claim of reference ambiguity.

## Clause-by-clause result

### Branching (`source/21-branching.md`)

| Requirement | Grade and observed evidence |
| --- | --- |
| ORNA-BRANCH-001 (`:14`) | **B.** `Repository::create_branch_at_head` documents a compare-and-set ref update without commit or worktree/index mutation (`crates/orna-repository-v1/src/lib.rs:3883`). `explicit_snapshot_branch_and_remote_preserve_cwd` checks current-HEAD target, index generation, and created ref (`crates/orna-repository-v1/tests/git_repository.rs:3184`). It does not check all CWD data and Git index bytes with pending staged and unstaged content on successful creation. |
| ORNA-BRANCH-002 (`:16`) | **B.** Same-commit checkout keeps staged, unstaged, and untracked Git state (`crates/orna-repository-v1/tests/git_repository.rs:2101`). This test does not attach a live Orna table overlay, allocator high-water mark, or source checkpoint; those parts remain unproven by this test. |
| ORNA-BRANCH-003 (`:18`) | **A.** Divergent checkout tests cover safe carry-forward, candidate validation failure, changed-state revalidation, Git conflicts, and untracked target protection (`crates/orna-repository-v1/tests/git_repository.rs:2168`, `:2212`, `:2260`, `:2300`, `:2329`). These establish the exercised repository and candidate paths, not every logical schema/row invalidation. |
| ORNA-BRANCH-004 (`:20`) | **B.** Unborn `HEAD` branch creation fails without changing `HEAD`, refs, index, status, or files (`crates/orna-repository-v1/tests/git_repository.rs:3210`). The distinct `orna switch -c` behavior is delegated by this clause to the administration chapter and is outside these two audited chapter clauses. |

### Evolution and semantic diff (`source/25-evolution.md`)

| Requirement | Grade and observed evidence |
| --- | --- |
| ORNA-MERGE-001 (`:5`) | **B.** Public `orna-evolution-v1::plan` derives ordered operations from two schema snapshots (`crates/orna-evolution-v1/src/lib.rs:275`); it is a planner, not an integrated database migration executor. |
| ORNA-MERGE-002 (`:7`) | **B.** The planner represents declarative changes as operations without a separate SQL migration input (`crates/orna-evolution-v1/src/lib.rs:118`, `:275`). No end-to-end source-to-stored-row migration test was found in this crate. |
| ORNA-SCHEMA-001 (`:11`) | **A.** Stable ObjectId field rename is asserted by `stable_field_identity_renames_and_key_removal_is_rejected` (`crates/orna-evolution-v1/src/lib.rs:878`). |
| ORNA-SCHEMA-002 (`:13`) | **A.** Replacing field identity plans delete plus add (`crates/orna-evolution-v1/src/lib.rs:856`). |
| ORNA-SCHEMA-003 (`:45`) | **B.** Optional addition yields a planner operation (`crates/orna-evolution-v1/src/lib.rs:802`); that test does not exercise legacy row reads through storage. |
| ORNA-SCHEMA-004 (`:47`) | **A.** Existing introduction fallback changes/removal fail (`crates/orna-evolution-v1/src/lib.rs:944`); typed fallback acceptance and operation shape are also checked (`:992`, `:1034`, `:1066`). |
| ORNA-SCHEMA-005 (`:49`) | **B.** Planner accepts only canonical closed scalar fallback values and rejects custom-type fallback (`crates/orna-evolution-v1/src/lib.rs:372`, `:404`). The planner receives a resolved value; evaluation/purity analysis of a source default expression is not represented by this API and is not claimed here. |
| ORNA-SCHEMA-006 (`:51`) | **B.** Incompatible required additions reject without a plan (`crates/orna-evolution-v1/src/lib.rs:836`); a direct public fixture case is added in this change. |
| ORNA-SCHEMA-007 (`:53`) | **C.** The planner models computed fields separately (`crates/orna-evolution-v1/src/lib.rs:70`, `:77`) but the checked-in planner tests do not directly assert that computed fields cannot serve as stored-data backfill. The table chapter owns the referenced computed-field semantics. |
| ORNA-SCHEMA-008 (`:59`) | **C.** This clause concerns stored-reference delete/re-key behavior. The evolution planner accepts explicit re-key intents (`crates/orna-evolution-v1/src/lib.rs:105`, `:520`); no direct integration proof for default restrict behavior was found in the inspected evolution/repository tests. |
| ORNA-MERGE-003 (`:81`) | **G.** No Git three-way merge path was found that skips equal table trees/segments before row comparison. Manifest/object digest verification exists for repository integrity, which is not this merge optimization. |
| ORNA-MERGE-004 (`:83`) | **G.** No changed/overlapping-key-range Git merge traversal was found. |
| ORNA-MERGE-005 (`:85`) | **B.** Compact logical-generation code enforces a key budget (`crates/orna-storage-v1/src/compact.rs:1770`, `:2932`), but an integrated merge conflict-count budget, affected table/range report, and unchanged-CWD proof were not found. The full clause is not established. |
| ORNA-MERGE-006 (`:87`) | **G.** No Git three-way schema merge for distinct optional columns was found. |
| ORNA-MERGE-007 (`:89`) | **G.** No Git three-way incompatible-column-type conflict path was found. |
| ORNA-MERGE-008 (`:91`) | **G.** No three-way keyed-row merge for independent field edits was found. |
| ORNA-MERGE-009 (`:93`) | **G.** No same-field three-way conflict path or explicit type-specific merge dispatch was found. |
| ORNA-MERGE-010 (`:95`) | **G.** No Git three-way generated-key/automatic-ID collision handling was found. |
| ORNA-MERGE-011 (`:97`) | **G.** No divergent opaque-checkpoint merge conflict path was found. |
| ORNA-DIFF-001 (`:118`) | **G.** CLI `diff` delegates to native Git diff (`crates/orna-cli-v1/tests/git_diff.rs:25`); no semantic schema/keyed-row/result/dependency diff report was found. |
| ORNA-DIFF-002 (`:120`) | **A.** `diff_streams_native_output_and_preserves_exit_code_for_paths_with_spaces` compares CLI output and exit codes with Git (`crates/orna-cli-v1/tests/git_diff.rs:25`). |
| ORNA-DIFF-003 (`:122`) | **C.** Compact storage has logical key/row merge and immutable-object distinctions, but no user-facing diff classification between physical rewrites and logical data changes was found in the targeted CLI/repository search. |

The storage function `merge_compact_loose` (`crates/orna-storage-v1/src/lib.rs:299`) merges two physical storage views by key; it is not the three-way Git/schema/row merge described by MERGE-1. Similarly, compact key-budget enforcement does not satisfy the complete merge-level resource/conflict reporting contract by itself.

## Five fixture-backed evidence gaps addressed

Existing in-module planner tests cover these transitions, but they were not exposed as a dedicated fixture-driven integration test against the crate's public API. The new `crates/orna-evolution-v1/tests/reference_schema_conformance.rs` tests load all schema input from `include_str!("fixtures/reference-schema-cases.json")` and assert:

1. ORNA-SCHEMA-001: stable identity turns a rename into `RenameField`.
2. ORNA-SCHEMA-002: changed identity turns a same-name replacement into delete plus add.
3. ORNA-SCHEMA-003: optional addition emits an optional-field plan operation, with no row-backfill operation in that plan.
4. ORNA-SCHEMA-004: a closed introduction fallback is planned and cannot later be changed.
5. ORNA-SCHEMA-006: a required stored field without fallback is rejected.

These tests add integration-level evidence for the planner boundary; they do not claim data rewrite behavior or close the Git merge and semantic-diff gaps above. The fixture contains schema metadata only, not Orna source code.

Focused CLI evidence, captured after adding the cases:

```text
$ cargo test --offline -p orna-evolution-v1 --test reference_schema_conformance
running 5 tests
test frozen_introduction_fallback_cannot_be_revised ... ok
test stable_object_identity_renames_the_field ... ok
test optional_field_addition_plans_no_row_backfill_operation ... ok
test rename_without_identity_continuity_is_delete_and_add ... ok
test required_field_without_fallback_is_rejected ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
exit code: 0
```

This run executes the new planner integration tests only. Existing branching and implementation-gap classifications above are based on static inspection of the named tests and code; this command does not execute the repository checkout suite.

## Targeted source-search evidence and scope limits

Audit searches at the stated base included `rg -n 'merge.?[0-9]?|CheckpointConflict|SemanticDiff|semantic_diff|physical.*logical|logical.*physical|three.?way|digest.*subtree|conflict_count'` across evolution, repository, CLI, application and storage implementation, plus focused test-name searches in repository and CLI integration suites. Results found schema planning, compact storage-level key merging/budgets, checkout tests, and native Git diff; they did not find a branch-aware semantic three-way merge or user-facing semantic diff layer. This is a bounded source audit, not a claim that no related code exists anywhere in the repository.

No Orna 1.0.0 behavior in these chapters was classified as reference-undefined. Where a production surface is absent, this report records it as a reference-defined implementation gap; it does not invent an alternative behavior. No source implementation or existing test file was modified. The separate dirty `evolution-domain-8ivy` worktree's `crates/orna-evolution-v1/src/lib.rs` change was preserved.
