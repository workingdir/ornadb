# Publication and repository conformance audit

Audit basis: frozen Orna 1.0.0 reference at `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, compared with the repository at the audit branch base and its checked-in implementation/tests. This is a bounded evidence review, not a claim that every requirement has been executed or fully proven.

## Evidence grades

- **A — direct:** a focused test asserts the named behavior.
- **B — bounded:** implementation or tests cover only part of the normative condition, or the evidence is indirect.
- **C — no direct proof found:** the audit did not find a test that establishes the criterion; this does not by itself prove the implementation is absent or wrong.

## Requirement map

| Reference clauses | Grade | Evidence and remaining limit |
|---|---:|---|
| `source/19-repository.md:14` ORNA-REPO-001; `:16` ORNA-REPO-002; `:18` ORNA-REPO-003; `:20` ORNA-REPO-004 | A | `crates/orna-project-v1/tests/project_loader.rs` checks root module loading, reachable module ordering, exclusion of unreachable modules, and row discovery only for reachable tables (`loads_only_reachable_modules_in_deterministic_logical_order`, `discovers_only_reachable_table_rows_with_opaque_path_metadata`). |
| `source/19-repository.md:34` ORNA-STATE-001; `:36` ORNA-STATE-002; `:48` ORNA-STATE-003; `:50` ORNA-STATE-004; `:52` ORNA-STATE-004A; `:54` ORNA-STATE-005 | B | Snapshot loading and committed/private candidate boundaries are covered in `project_loader.rs`; repository snapshot tests cover Git-backed identities. This audit found no single end-to-end test spanning all selectors, schema pinning, historical function-code behavior, and `now()` distinction. |
| `source/19-repository.md:70` ORNA-META-001; `:72` ORNA-META-002; `:74` ORNA-META-003; `:76` ORNA-META-004; `:78` ORNA-META-005 | B | `crates/orna-cli-v1/tests/init.rs` checks stable database identity and initialization; `project_loader.rs` checks committed metadata and private candidate behavior. The audit did not find a complete test proving every metadata/cache exclusion and watermark rule across ordinary commits. |
| `source/19-repository.md:84` ORNA-LOCAL-001; `:96` ORNA-LOCAL-002; `:98` ORNA-LOCAL-003; `:100` ORNA-LOCAL-004 | A/B | `crates/orna-repository-v1/tests/git_repository.rs` tests Git-resolved and linked-worktree runtime directories. New `crates/orna-runtime-v1/tests/publication_repository_conformance.rs` adds direct checks for runtime database placement and ordinary Git-state exclusion. Cache rebuildability remains grade C: no direct rebuild test was found. |
| `source/19-repository.md:104` ORNA-EMBED-001; `:106` ORNA-EMBED-002; `:108` ORNA-EMBED-003; `:110` ORNA-EMBED-004 | B/C | The runtime opens in-process in the new tests, and repository paths are worktree-scoped. The audit found no direct competing-process/socket coordination or fault proof establishing the no-experimental-multi-process-correctness boundary; the socket behavior is optional and remains implementation-defined. |
| `source/19-repository.md:116` ORNA-CLONE-001; `:118` ORNA-CLONE-002; `:120` ORNA-CLONE-003; `:122` ORNA-CLONE-004 | A/B | New tests prove separate runtime tails for linked worktrees and clones. `crates/orna-repository-v1/tests/git_transport.rs` covers Git clone/fetch/push behavior. This audit found no direct test for same-consumer exclusion or the explicit absence of cross-clone exactly-once guarantees; no exactly-once behavior is claimed here. |
| `source/19-repository.md:127` ORNA-CORE-001; `:129` ORNA-CORE-002; `:131` ORNA-CORE-003 | B | Git repository and committed snapshot tests exercise ordinary Git objects and metadata. The audit found no one test proving a full snapshot can reproduce all code, schema, large-table objects, and watermarks together. |
| `source/22-publication.md:7` ORNA-HOT-001; `:9` ORNA-HOT-002; `:11` ORNA-PUB-001; `:13` ORNA-PUB-002; `:15` ORNA-PUB-003; `:17` ORNA-PUB-004 | A/B | Runtime publication metadata tests cover effective policy, pending/published metadata, and `sys.Storage`; CLI status tests cover summary output. Storage policy tests exercise documented bounds. Automatic age-triggered smaller-batch publication is permitted, not required, and is not claimed by this audit. |
| `source/22-publication.md:27` ORNA-PUB-010; `:29` ORNA-PUB-011; `:31` ORNA-PUB-012 | A | Repository tests cover private candidate construction, preservation of ordinary CWD state, index reconciliation, and pauses for index locks and unfinished merge/rebase states. See `git_repository.rs` tests `private_candidate_uses_head_and_preserves_ordinary_cwd_state`, `published_candidate_reconciles_only_managed_index_entries`, and `publication_pauses_*`. |
| `source/22-publication.md:49` ORNA-PUB-005; `:51` ORNA-PUB-006; `:53` ORNA-PUB-007; `:55` ORNA-PUB-008; `:57` ORNA-PUB-009; `:59` ORNA-PUB-016; `:61` ORNA-PUB-017 | A/B | Existing repository tests cover pre/post-ref recovery, compare-and-set advancement, unrelated partial staging, conflict preservation, and a normal commit after publication. Evidence is broad but distributed across focused tests, not one exhaustive crash matrix. |
| `source/22-publication.md:78` ORNA-PUB-013; `:80` ORNA-PUB-014; `:82` ORNA-PUB-015 | B/C | `publication_metadata.rs` and status tests cover remaining unpublished state. This audit found no full child-shutdown-to-publication test and no end-to-end resumable transfer between two clones. The new clone test demonstrates independent tails only; it does not claim transfer or recovery after loss. |

## Top five bounded proof gaps and added tests

The following new tests close five focused evidence gaps without changing production behavior:

1. Runtime state opens beneath Git's resolved worktree administration path: `runtime_state_opens_under_git_resolved_worktree_administration` (ORNA-LOCAL-001, ORNA-EMBED-002).
2. Linked worktrees maintain separate pending tails: `linked_worktrees_keep_separate_runtime_tails` (ORNA-LOCAL-001, ORNA-LOCAL-002, ORNA-CLONE-001).
3. Clones maintain independent CWD tails: `cloned_repositories_keep_independent_runtime_cwds` (ORNA-CLONE-001, ORNA-CLONE-002).
4. An unpublished runtime tail does not become ordinary Git content: `unpublished_runtime_tail_is_absent_from_ordinary_git_state` (ORNA-META-001, ORNA-LOCAL-004, ORNA-CORE-001).
5. The unpublished tail remains durable across closing and reopening runtime state: `durable_unpublished_tail_survives_runtime_reopen` (ORNA-LOCAL-002, ORNA-HOT-001).

All Orna source used by these tests is loaded from the real `crates/orna-runtime-v1/tests/fixtures/publication-repository-main.orna` file with `include_str!`. These tests do not establish runtime tail transfer, cache rebuildability, competing-process coordination, or the complete reproducibility guarantee; those remain the bounded C/B items above.

## Frozen evidence register

The reference's `tests/requirement-evidence.json` records `implementation_result: not executed` and `full_implementation_coverage_claimed: false` for its entries. This audit does not alter that frozen corpus or upgrade its results. The test run for this change is recorded in the PR and issue evidence.
