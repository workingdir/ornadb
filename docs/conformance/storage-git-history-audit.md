# Storage and Git-history conformance audit

Reference: frozen OrnaDB 1.0.0 `source/23-storage.md` and
`source/24-git-history.md`. The line anchors below refer to that reference.

Evidence grades:

- **A** — implementation and focused executable test source found. Test source
  alone does not establish a pass; see the captured command result for whether
  it ran.
- **B** — implementation path and related tests found, but no isolated
  requirement-level implementation proof was found.
- **GAP** — normative implementation or executable evidence is absent from the
  audited storage/transport surfaces.
- **OUT** — the clause assigns behavior to another layer; its implementation
  cannot be established from `orna-storage-v1` or Git transport alone.

The reference evidence register at `tests/requirement-evidence.json` labels
these chapter obligations as planned implementation-conformance obligations;
that register is a test plan, not a result. The new integration file
`crates/orna-repository-v1/tests/storage_git_history_conformance.rs` adds five
fixture-backed scenarios at the storage/Git boundary. Each Orna input comes
from the checked-in `git-repository-main.orna` or
`git-repository-nested-tool.orna` fixture via `include_str!`.

## Chapter 23 — physical storage and representation

| Reference requirement | Grade | Tip evidence or precise gap |
|---|---:|---|
| ORNA-STORAGE-001, source/23-storage.md:5 | B | Compact encoding and repository publication have separate tests; one common table/query/mutation interface across profiles was not found in the audited crate surface. |
| ORNA-STORAGE-002, source/23-storage.md:7 | GAP | No profile-independent table query entry point was found in `orna-storage-v1`. |
| ORNA-STORAGE-003, source/23-storage.md:13 | B | Managed-path validation exists in `orna-storage-v1/src/lib.rs`; no chapter-specific row-path conformance test was identified. |
| ORNA-STORAGE-004, source/23-storage.md:15 | A | New test checks a real fixture edit through ordinary `git diff` and verifies the previous blob remains readable from its commit. |
| ORNA-COMPACT-001, source/23-storage.md:21 | OUT | Compact key decoding and publication are present; logical query, mutation, history and merge parity is owned by higher table/repository layers. |
| ORNA-COMPACT-002, source/23-storage.md:23 | B | Parquet reader validation and publication tests cover page checksums, encoding, sorting and physical metadata; no one cross-reader suite was identified. |
| ORNA-COMPACT-003, source/23-storage.md:25 | A | Stable field identity, schema descriptors and physical mapping are checked by compact typed-lowering and repository publication tests. |
| ORNA-COMPACT-004, source/23-storage.md:27 | A | Repository publication test `compact_publication_excludes_computed_fields_and_deletion_non_keys` checks computed-field exclusion. |
| ORNA-COMPACT-005, source/23-storage.md:29 | B | Publication target bounds are represented by `publication_policy.rs`; full row-group/age closure conformance was not located. |
| ORNA-COMPACT-006, source/23-storage.md:31 | A | Generation allocation/retry, recovery and mutation role behavior have focused repository publication tests. |
| ORNA-COMPACT-007, source/23-storage.md:33 | GAP | No explicit consolidation API, history-cost preview or semantic-empty-diff proof was found. |
| ORNA-COMPACT-008, source/23-storage.md:35 | A | Canonical manifest/shard validation and the 256-entry boundary have repository tests. |
| ORNA-COMPACT-009, source/23-storage.md:37 | B | Exact key range hydration and Parquet metadata bounds are tested; a complete query planner proving shard/file/row-group/page pruning and projection was not found. |
| ORNA-COMPACT-010, source/23-storage.md:39 | A | Unknown profile recovery fails closed in `git_repository.rs`; encoder/profile validation is also exercised in compact reader tests. |
| ORNA-COMPACT-011, source/23-storage.md:41 | A | Publication journals, ordinary-index reconciliation, crash recovery and stale-HEAD rebuild have focused repository tests. |
| ORNA-COMPACT-012, source/23-storage.md:43 | A | Decimal, UUID, Instant and OVB fallback suites cover exact encodings and malformed input rejection. |
| ORNA-COMPACT-013, source/23-storage.md:45 | GAP | Qualification validators exist, but no published production benchmark/fault evidence was found; the production compact-storage claim is not established by this audit. |
| ORNA-COMPACT-014, source/23-storage.md:47 | B | Publication freeze/lowering and generation recovery are tested; no direct test was found for every frozen-batch mutation chain. |
| ORNA-COMPACT-015, source/23-storage.md:49 | A | Stale-head publication rebuild is covered by `compact_publication_rebuilds_from_the_current_manifest_after_a_stale_head`. |
| ORNA-COMPACT-016, source/23-storage.md:51 | B | Repository semantic merge has row conflict coverage; generation normalization after merge was not isolated in a requirement-level test. |
| ORNA-COMPACT-017, source/23-storage.md:53 | B | Float total-order and NaN-statistics code exists in `compact_parquet.rs`; focused assertions are in module tests rather than a new external fixture suite. |
| ORNA-COMPACT-018, source/23-storage.md:55 | B | Schema identity and mapping validation are tested; cross-snapshot projection for renamed, optional, frozen-fallback and computed fields was not located as a whole. |
| ORNA-COMPACT-019, source/23-storage.md:57 | B | Committed manifest validation rejects malformed/duplicate state; branch-input same-generation merge cases are not comprehensively isolated. |
| ORNA-STORAGE-005, source/23-storage.md:65 | A | Exact editable/compact overlap errors and publication checks are covered in compact key and repository integration tests. |
| ORNA-STORAGE-006, source/23-storage.md:67 | GAP | Compact mutation roles are implemented; full table-API preservation of editable placement and compact replacement/deletion is not exposed by the audited storage API. |
| ORNA-STORAGE-007, source/23-storage.md:69 | GAP | No committed placement-preference behavior was found beyond storage metadata observation. |
| ORNA-STORAGE-008, source/23-storage.md:71 | GAP | Automatic placement threshold behavior (row count, canonical body size, path representability and compact-once-present) was not found in the audited implementation. |
| ORNA-STORAGE-009, source/23-storage.md:73 | GAP | Editable/compact preference-directed insertion is not implemented in the audited storage API. |
| ORNA-STORAGE-010, source/23-storage.md:75 | GAP | `orna-sys-v1` exposes reference descriptors, but no runtime dispatch for `sys.admin.set_storage_preference` or `sys.admin.rewrite_storage` was found. |
| ORNA-STORAGE-011, source/23-storage.md:77 | GAP | No recoverable editable/compact rewrite operation was found. |
| ORNA-STORAGE-012, source/23-storage.md:79 | OUT | `sys.Storage` observation exists; profile-independent ordinary table reads/writes belong to the table/runtime layer. |
| ORNA-STORAGE-013, source/23-storage.md:81 | B | Exact disjointness is checked in key validation and publication; exhaustive coverage of every listed boundary, especially checkout, re-key and semantic merge, was not established. |
| ORNA-STORAGE-014, source/23-storage.md:83 | A | Compact exact-key lookup is available; manifest-range hydration tests select only overlapping segment candidates and preserve Git state. |
| ORNA-STORAGE-015, source/23-storage.md:85 | B | Duplicate detection and publication conflicts are tested; all listed detection boundaries and shadow-barrier equivalence were not proven together. |
| ORNA-STORAGE-FORMAT-001, source/23-storage.md:208 | A | Incomplete, incompatible and identity-substituted descriptors are rejected in compact reader and repository publication tests. |
| ORNA-STORAGE-FORMAT-002, source/23-storage.md:210 | B | Canonical OVB and logical schema fingerprints are used; alternate physical encoders producing equal logical hashes were not demonstrated in a cross-reader test. |

The principal production gap in this chapter is the table placement/rewrite
surface: reference descriptors exist, but the runtime does not dispatch the
two specified administration operations. This audit does not invent their
transaction or return semantics beyond the cited requirements.

## Chapter 24 — Git history, remotes and partial clones

| Reference requirement | Grade | Tip evidence or precise gap |
|---|---:|---|
| ORNA-GIT-001, source/24-git-history.md:5 | A | New fixture-backed test verifies complete committed source content and reads the prior snapshot by commit ID. |
| ORNA-GIT-002, source/24-git-history.md:7 | B | Partial-clone observation and promised-object tests exist; the new test covers capability distinction, not omitted-blob reachability. |
| ORNA-GIT-003, source/24-git-history.md:9 | A | Snapshot closure and promised-vs-unavailable object tests reject incomplete logical availability. |
| ORNA-GIT-004, source/24-git-history.md:21 | A | New test verifies combined sparse-checkout and partial-clone capability flags remain distinct. |
| ORNA-GIT-005, source/24-git-history.md:23 | B | Manifest-bound selective hydration is tested; complete query-planner selection remains a gap recorded under ORNA-COMPACT-009. |
| ORNA-GIT-006, source/24-git-history.md:25 | B | Promisor reachability and targeted hydration are tested; lazy fetch authorization is limited to the explicit hydration operation. |
| ORNA-GIT-007, source/24-git-history.md:31 | B | Transport uses ordinary configured remotes and tests use local/bare remotes; no host-specific large-object-promisor requirement was found. |
| ORNA-GIT-008, source/24-git-history.md:33 | B | Multiple configured remotes and promised-object reachability are supported by Git; separate large-object-promisor topology has no dedicated conformance test. |
| ORNA-REMOTE-001, source/24-git-history.md:44 | B | Transport uses ordinary Git remotes; remote URL relocation is covered by an existing fetch test. |
| ORNA-REMOTE-002, source/24-git-history.md:46 | B | No `remote migrate` command was found; ordinary `remote set-url` remains available. |
| ORNA-REMOTE-003, source/24-git-history.md:48 | A | New tests exercise clone, fetch and push of internal refs along with ordinary refs. |
| ORNA-REMOTE-004, source/24-git-history.md:50 | A | New push test records the bare remote update hook and checks allocator ref advancement precedes branch visibility. |
| ORNA-REMOTE-005, source/24-git-history.md:52 | B | Passive continuity observation reports missing/stale refs; the new test set does not exercise a plain-Git transferred repository's allocation boundary. |
| ORNA-GIT-009, source/24-git-history.md:56 | A | New history test proves an earlier fixture snapshot remains reachable and readable after a later commit. |
| ORNA-GIT-010, source/24-git-history.md:58 | B | No implicit Orna history-rewrite operation was found; explicit destructive-operation diagnostics are not established because no such Orna operation was located. |

## Disposition

The implementation does **not** validate full chapter-23 conformance. The
compact encoding/publication core and Git transport show substantial tested
coverage, but storage placement/rewrite behavior (ORNA-STORAGE-006 through
ORNA-STORAGE-011) and compact consolidation/planner/production-evidence
obligations (ORNA-COMPACT-007, 009 and 013) remain gaps. Git-history transport
is substantially implemented; the new file closes five focused integration
evidence gaps, subject to its captured test result. No behavior was added for
reference clauses whose implementation surface is absent.
