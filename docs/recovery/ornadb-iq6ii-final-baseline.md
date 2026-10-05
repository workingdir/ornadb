# ornadb-iq6ii: protected-WIP recovery against final format 3

Date: 2026-10-05
Recovery branch: `herdr/final-wip-recovery`
Issue mapping: Beads `ornadb-iq6ii`; GitHub issue [#8217](https://github.com/workingdir/ornadb/issues/8217)

## Outcome

No production transplant is safe from the inspected WIP. The WIP is a
cross-layer, unresolved merge whose storage/runtime portion implements the
superseded compact/Parquet and loose-row model. The final publication requires
one format-3 writer and one shared indexed store. A partial transplant would
either reintroduce a retired writer or require protected runtime, manifest and
lock changes. This commit is therefore a material recovery report, not a
salvage claim.

## Authorities and comparison baseline

- Final reference resolved locally as
  `/home/pbox/dev/ornadb/reference/Orna-1.1.0`; the literal requested path
  `/home/pbox/dev/ornadb/reference/Orna-1.1.0 final-2026-10-05` is not present.
- `release.json` reports publication `final-2026-10-05`, repository writer
  format `3`, and coordinates OGS-1 / ORP-1 / OGB-2 / ROV-3 / SOV-3 / OVB-2 /
  VFS-1 / MIME-1. It retains repository formats 1 and 2 as read-only legacy
  contexts.
- All 395 payload entries in the local `SHA256SUMS` verification passed.
- The final archive
  `/home/pbox/dev/ornadb/Orna-1.1.0-final-20261005.zip` independently hashes
  to `fe4a35dc81c2c2994e0a18d7c7931ea0bbd298ab23248496cbe880f264c5db56`.
- Every repository comparison below uses only `origin/main` at
  `6a89d42879782ec63bed060b283baf50394be134`.

Required guidance was read before this decision: `START-HERE.md`,
`release.json`, `PLAN.md`, `WALKTHROUGH.md`, `implementation/ORCHESTRATOR.md`,
`implementation/ACCEPTANCE.md`, and `source/manifest.json`. Relevant ordered
chapters read were 03, 05–08, 10, 15, 18–25, 29–33, and 36–42, plus the
format-3 coordinates/store profiles. The decisive requirements are:

- ORNA-ROW-001..015 and ORNA-UPGRADE-001..010: one ORP-1 writer; format-1/2
  readers remain versioned and read-only.
- ORNA-META-001..005 and ORNA-PUB-010..012: `.orna/database.orna` plus the
  native `.orna/store`, private local pending state, and conflict-pausing
  publication.
- ORNA-SYS-115..120: one recorded storage profile; retired placement APIs are
  rejected rather than selecting a hidden writer.
- ORNA-PROTO-001..004 and ORNA-BLOB-007..021: preserve legacy OVB-1/live
  behavior and keep new annotated values in explicit v2/v3 contexts.
- ORNA-CAP-001..006, ORNA-TXN-001..008 and ORNA-CHECKOUT-001..004: durable
  ownership, transaction, object-retention and conflict-preservation
  boundaries.

## Protected checkout inventory

Protected worktree: `/home/pbox/dev/ornadb/work` (read-only inspection only).

At the preservation checkpoint it was:

```text
branch: main...origin/main [ahead 1, behind 4131]
HEAD:   c286bdb98d88ef94f1f0bca1ce4bd1de495c9896
base:   6a89d42879782ec63bed060b283baf50394be134
status: A 4672, D 10, M 156, R 1, UU 20
index-vs-origin: A 110, D 1896, M 129, R052 1, R099 1, U 20
top-level index-vs-origin: crates 1939, playground 101, docs 78,
  editors 16, ornadb 7, scripts 3, packaging 2, api 2, .github 2,
  stdlib 1, plus root files
```

The full tracked worktree/index path set was inspected with read-only
`git status`, `git diff --name-status origin/main`,
`git diff --cached --name-status origin/main`, `git ls-files -u`, and targeted
blob reads. The exact unresolved path set was:

```text
crates/orna-cli-v1/src/main.rs
crates/orna-conformance-v1/tests/reference_corpus.rs
crates/orna-conformance-v1/tests/semantic_runtime_adapter.rs
crates/orna-evaluator-v1/src/lib.rs
crates/orna-evaluator-v1/tests/evaluator.rs
crates/orna-live-v1/src/lib.rs
crates/orna-project-v1/tests/project_loader.rs
crates/orna-repository-v1/src/compact.rs
crates/orna-runtime-v1/src/activation.rs
crates/orna-runtime-v1/src/lib.rs
crates/orna-semantic-v1/tests/imported_helper_dependencies.rs
crates/orna-semantic-v1/tests/semantic_graph.rs
crates/orna-storage-v1/src/compact.rs
crates/orna-storage-v1/src/compact_parquet.rs
crates/orna-storage-v1/src/lib.rs
crates/orna-sys-v1/src/lib.rs
crates/orna-sys-v1/tests/explain_diagnostic.rs
crates/orna-traceability-v1/tests/engine_witness_binding.rs
crates/orna-traceability-v1/tests/traceability_integrated_evidence.rs
crates/orna-traceability-v1/tests/traceability_status_boundaries.rs
```

The exact protected no-touch infrastructure paths observed in the focused
inventory were `.gitmodules`, `Cargo.toml`, `Cargo.lock`, `api/sys.json`,
`api/sys.schema.json`, `crates/orna-runtime-v1/**`,
`crates/orna-repository-v1/**`, and `crates/orna-storage-v1/**`. The index also
contained new `crates/orna-compiler/**` and `crates/orna-syntax/**` trees whose
integration requires workspace manifest changes. None of these paths was
copied into this branch.

## Per-conflict disposition

| Protected path(s) | WIP finding | Final mapping and recovery decision |
| --- | --- | --- |
| `crates/orna-repository-v1/src/compact.rs` | Stage-2 WIP is explicitly a compact-manifest/Parquet publication witness. | Contradicts ORNA-ROW-001..010 and source 23: Parquet/compact is not a format-3 writer. Do not transplant. Revisit only as a legacy reader/interchange adapter with a separate format dispatch proof. |
| `crates/orna-storage-v1/src/compact.rs`, `compact_parquet.rs`, `lib.rs` | WIP exposes `compact-storage-v1`, loose rows, compact bases, tombstones and Parquet key sources. | Directly contradicts the single ORP-1 writer, native OGS-1 graph and no loose/compact/hybrid new writer rules. Do not transplant any production or test slice. |
| `crates/orna-runtime-v1/src/activation.rs`, `src/lib.rs` | Runtime WIP couples pending state to `CompactPublicationPending`, compact receipts, loose rows/tombstones and compact watermarks. | Shared runtime ownership crosses ORNA-TXN, ORNA-LOCAL, ORNA-PUB and ORNA-CHECKOUT boundaries; the storage model is retired. Do not resolve or transplant in recovery. |
| `crates/orna-sys-v1/src/lib.rs` | WIP contains `sys.admin.compact`, `sys.admin.set_storage_preference` and `sys.admin.rewrite_storage` descriptors. | Contradicts ORNA-SYS-115..120 and source 23/`api/retired.json`: no placement selector or hidden future writer. Do not transplant. |
| `crates/orna-cli-v1/src/main.rs` | Large WIP CLI/session change is conflict-coupled to evaluator, runtime and system changes. | Must be reimplemented or rebased against source 18's final CLI surface, including format-3 row staging, mount/edit, upgrade and export. No isolated transplant. |
| `crates/orna-evaluator-v1/src/lib.rs`, `tests/evaluator.rs` | Broad evaluator/relation/cancellation rewrite; production file is conflicted. | Crosses ORNA-TXN-005/006, ORNA-REL-001..006 and ORNA-CANCEL-001..002. Tests cannot certify a final format-3 storage path. No transplant. |
| `crates/orna-live-v1/src/lib.rs` | Broad `orna.present.v1` session ownership/recovery change; production file is conflicted. | Legacy OVB-1 protocol must remain exact while new value profiles negotiate explicitly under ORNA-PROTO-001..004 and ORNA-BLOB-018. No partial transplant without an integrated protocol review. |
| `crates/orna-semantic-v1/tests/imported_helper_dependencies.rs`, `tests/semantic_graph.rs` | Test-only semantic/import changes are conflicted. | Source 03/05/06/07 rules require full parser/resolver/type/effect validation; no production artifact is recoverable from these tests. Do not transplant. |
| `crates/orna-conformance-v1/tests/reference_corpus.rs`, `tests/semantic_runtime_adapter.rs` | Test-only changes add or alter semantic/runtime evidence. | Source 32 distinguishes authored cases, model checks and executed engine evidence. These tests do not establish final storage/runtime conformance and are not production salvage. |
| `crates/orna-project-v1/tests/project_loader.rs` | Test-only project-loading changes are conflicted. | Source 03 and ORNA-TEST-003/009 require reachable-module admission and declared legacy-row input context; preserve for a future rebased test audit only. |
| `crates/orna-sys-v1/tests/explain_diagnostic.rs` | Test-only system diagnostic conflict. | ORNA-SYS-073/103–141 require exact API/profile evidence; no isolated test transplant is justified. |
| `crates/orna-traceability-v1/tests/engine_witness_binding.rs`, `traceability_integrated_evidence.rs`, `traceability_status_boundaries.rs` | Test-only traceability conflicts. | Final evidence must distinguish specified/model-only from executed implementation evidence. Preserve as historical WIP candidates, not salvage. |

## Non-conflicting candidate audit

The apparently small production deltas were also read against `origin/main`:

- `orna-client/src/live_transport.rs`, `orna-core/src/system.rs` and
  `orna-evolution-v1/src/lib.rs` are whitespace/comment-only changes; making a
  commit from them would be fake salvage.
- `orna-evaluator-v1/src/cancellation.rs` removes parent cancellation
  propagation, which is not independently safe under ORNA-CONCUR-001 and
  ORNA-CANCEL-002.
- `orna-evaluator-v1/src/repl.rs` removes a project-REPL alias path and changes
  feature gating; it is coupled to the conflicted CLI/project/evaluator graph.
- `orna-lsp/src/lib.rs` and `documents.rs` switch to the WIP-only
  `orna-syntax` crate and remove existing modules; this requires new crates and
  protected workspace-manifest changes.
- `orna-semantic-v1/src/repl.rs`, `src/system_api.rs` and
  `orna-sys-macros/src/lib.rs` remove or rename API surfaces as part of the
  same refactor; they are neither format-3 storage work nor independently
  reviewable.
- `orna-traceability-v1/src/lib.rs` removes its embedded API fallback, but its
  companion `Cargo.toml` removes the `orna-sys-v1` dependency and its test is
  deleted. That is manifest-coupled evidence-tool behavior, not production
  salvage.
- The WIP-only `orna-compiler/**` and `orna-syntax/**` trees are broad new
  compiler/frontend additions. They require protected workspace integration and
  do not implement the final OGS-1/ORP-1/OGB-2 storage contract.

No candidate passed all three gates: independently reviewable, conforming to
the final format-3 authority, and integrable without protected shared
runtime/manifest/lock changes.

## Preservation and evidence

- The protected checkout was never reset, cleaned, checked out, merged,
  rebased, staged, or written. No protected file was edited or copied.
- No Cargo command, heavy build, or broad test suite was run. Test evidence is
  therefore **not executed**, not passed or failed.
- The recovery worktree started at the exact requested `origin/main` commit and
  remained clean while the protected inventory was read.
- Legacy repository/value formats 1 and 2 were treated as read-only; no
  migration, writer, manifest, lockfile, runtime, or reference change was
  attempted.
- The only file added by this recovery branch is this report under
  `docs/recovery/`.

This report deliberately leaves the protected WIP available for a coordinator
to preserve or archive. Any future salvage should begin from a fresh
`origin/main` comparison, select one bounded production owner, and prove the
format-3 row/graph/value contract before changing shared runtime or manifests.
