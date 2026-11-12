# Crate split plan: measured, ordered slices

Status: plan. Filed under epic `crate-decomposition`. This document is measured
from the tree at `origin/main` 710ac17ff. It supersedes the split stages of the
paused reform epic `ornadb-7603h` (see section 7). It does not change any code.

## 1. Question and method

The owner wants the monolith crates broken into smaller crates that other
binaries and crates can reuse. The governing rules are
[ARCHITECTURE.md](../ARCHITECTURE.md) sections 4 to 7, the layer table in
[layers.toml](layers.toml), and the recorded debt in
[architecture-budget.toml](../../scripts/architecture-budget.toml).

Measurements, all reproducible from the tree:

- Per-crate size: `wc -l` over `src/**/*.rs` and `tests/**/*.rs`.
- Pub modules: `^pub mod` in each `src/lib.rs`.
- Dependents: `cargo metadata --locked` resolve graph, counting only normal
  (non-dev) edges between workspace members.
- Budget debt: `scripts/check_architecture.py --mode=budget` records 93 files
  over 1200 lines and 251 functions over 120 lines. Each file's test code is
  measured by the same stripping rule the checker uses: `#[cfg(test)] mod`
  blocks are counted as test lines.
- Compile-time share: not measured. `cargo build --timings` is too slow for a
  planning pass. LOC and dependent fan-in are the proxies.

Two measurement caveats, both material:

1. The budget counts test code. Inline `#[cfg(test)]` modules and `src/tests.rs`
   count toward the file's line total. Ten whole-file test modules
   (`src/**/tests.rs`, `src/**/tests/*.rs`, `*_tests.rs`) are not inline, so the
   per-file test stripping leaves them in the production column. They add 28,006
   lines. Counting them, test code is (122,751 + 28,006) / 353,090 = 42% of
   recorded budget lines, not 35%.
2. Tests in `tests/` directories are not budget-counted at all (the budget scans
   `src/` only). Per-crate `tests/` LOC is shown separately below.

## 2. Measurements

### 2.1 Workspace crates

Command: `cargo metadata --locked` plus `wc -l`. Sorted by source LOC. "Prod
dependents" is the number of workspace crates with a normal edge to this crate.

| crate | src files | src LOC | tests/ LOC | pub mods (lib.rs) | prod dependents |
|---|---:|---:|---:|---:|---:|
| orna-repository | 54 | 73443 | 18003 | 8 | 9 |
| orna-runtime | 33 | 68643 | 22914 | 1 | 6 |
| orna-compat-v1 | 35 | 60764 | 1344 | 19 | 0 |
| orna-cli | 53 | 42771 | 73066 | 0 | 0 |
| orna-check | 30 | 36650 | 27041 | 1 | 8 |
| orna-sys | 22 | 31318 | 63761 | 1 | 6 |
| orna-evaluator | 29 | 31317 | 52387 | 2 | 7 |
| orna-project | 7 | 22095 | 14605 | 0 | 5 |
| orna-application | 11 | 20727 | 2713 | 0 | 2 |
| orna-conformance | 7 | 20159 | 22190 | 1 | 2 |
| orna-hir | 74 | 19274 | 1802 | 7 | 6 |
| orna-value | 22 | 14052 | 25454 | 1 | 22 |
| orna-live | 2 | 13831 | 25124 | 1 | 2 |
| orna-exec | 79 | 13389 | 4433 | 1 | 3 |
| orna-syntax | 14 | 11464 | 20405 | 2 | 16 |
| orna-lsp | 21 | 11326 | 11558 | 0 | 0 |
| orna-storage | 9 | 10832 | 6403 | 0 | 2 |
| orna-intrinsics | 25 | 4639 | 1018 | 0 | 3 |
| orna-protocol | 1 | 4460 | 240 | 0 | 5 |
| orna-differential | 12 | 4354 | 1730 | 9 | 0 |
| orna-sys-codegen | 4 | 4089 | 0 | 3 | 0 |
| orna-traceability | 3 | 3858 | 1000 | 1 | 0 |
| orna-stream | 1 | 2847 | 0 | 0 | 3 |
| orna-table | 1 | 2829 | 0 | 0 | 1 |
| orna-project-repl | 1 | 2611 | 0 | 0 | 4 |
| orna-format3 | 6 | 2186 | 0 | 4 | 1 |
| orna-security | 6 | 1721 | 237 | 0 | 6 |
| orna-evolution | 1 | 1268 | 432 | 0 | 1 |
| orna-serving | 2 | 1244 | 451 | 1 | 3 |
| orna-sys-id | 12 | 1106 | 1816 | 11 | 7 |
| orna-repl | 9 | 1000 | 388 | 0 | 0 |
| orna-link | 5 | 995 | 634 | 0 | 1 |
| orna-sys-interface | 3 | 857 | 0 | 0 | 1 |
| orna-base | 4 | 341 | 0 | 1 | 14 |
| orna-native-loader | 4 | 333 | 142 | 0 | 0 |
| orna-sys-model | 4 | 265 | 0 | 3 | 2 |
| orna-standard-sources | 1 | 139 | 0 | 0 | 4 |
| orna-sys-macros | 1 | 19 | 0 | 0 | 1 |

38 workspace crates. `orna-base` has the most normal-edge fan-in per line
(14 dependents for 341 LOC). `orna-value` (22) and `orna-syntax` (16) are the
two largest shared hubs. `orna-value`'s 2,341-line `lib.rs` is mostly tests:
2,259 test lines, 82 production lines. The "2,334-line monolith" is a test
file, not a production module.

Dependents reported as 0 for binaries (`orna-cli`, `orna-lsp`, `orna-compat-v1`
by its archive role, `orna-differential`, `orna-repl`, `orna-native-loader`,
`orna-sys-codegen`, `orna-traceability`). Those are not orphan crates: the
checker treats binaries and declared roles separately.

### 2.2 Budget debt by crate

Command: `scripts/check_architecture.py --mode=budget` (recorded entries), each
file measured with inline test stripping. "Test" is inline `#[cfg(test)]` lines.

| crate | budget files | total lines | test lines | prod lines | test share |
|---|---:|---:|---:|---:|---:|
| orna-runtime | 12 | 58383 | 36700 | 21683 | 62% |
| orna-compat-v1 | 20 | 51887 | 8524 | 43363 | 16% |
| orna-repository | 11 | 50765 | 21729 | 29036 | 42% |
| orna-sys | 5 | 25850 | 745 | 25105 | 2% |
| orna-evaluator | 4 | 21882 | 5371 | 16511 | 24% |
| orna-project | 5 | 20622 | 11668 | 8954 | 56% |
| orna-conformance | 3 | 19455 | 3986 | 15469 | 20% |
| orna-application | 3 | 17848 | 11456 | 6392 | 64% |
| orna-cli | 7 | 15144 | 1857 | 13287 | 12% |
| orna-live | 2 | 13831 | 4499 | 9332 | 32% |
| orna-check | 4 | 12629 | 377 | 12252 | 2% |
| orna-storage | 2 | 8535 | 4619 | 3916 | 54% |
| orna-syntax | 2 | 8052 | 147 | 7905 | 1% |
| orna-value | 4 | 6252 | 2259 | 3993 | 36% |
| orna-protocol | 1 | 4460 | 2112 | 2348 | 47% |
| orna-lsp | 2 | 4228 | 895 | 3333 | 21% |
| orna-traceability | 2 | 3712 | 1250 | 2462 | 33% |
| orna-stream | 1 | 2847 | 1401 | 1446 | 49% |
| orna-table | 1 | 2829 | 1141 | 1688 | 40% |
| orna-project-repl | 1 | 2611 | 1373 | 1238 | 52% |
| orna-evolution | 1 | 1268 | 642 | 626 | 50% |
| **TOTAL** | **93** | **353090** | **122751** | **230339** | **34%** |

Whole-file test modules (not counted in the test column above, 28,006 lines in
10 files): `orna-compat-v1` (`canonical_hash/tests.rs`, `physical/tests.rs`,
`revision/tests.rs` and its `record_values.rs`, `security/tests.rs`,
`value/tests/constructed.rs`), `orna-repository`
(`compact/legacy_reader_tests.rs`), `orna-check` (`tests.rs`), `orna-sys`
(`tests.rs`), `orna-value` (`format3_context_tests.rs`).

### 2.3 Largest recorded files

Sorted by current total. Test and production split by inline `#[cfg(test)]`
stripping.

| crate | file | recorded | total | test | prod |
|---|---|---:|---:|---:|---:|
| orna-runtime | src/lib.rs | 35336 | 35451 | 32849 | 2602 |
| orna-sys | src/introspection.rs | 17855 | 17855 | 446 | 17409 |
| orna-evaluator | src/lib.rs | 15928 | 15793 | 3412 | 12381 |
| orna-application | src/lib.rs | 14830 | 14884 | 10360 | 4524 |
| orna-conformance | src/semantic_adapter.rs | 14479 | 14501 | 3455 | 11046 |
| orna-repository | src/native_graph.rs | 14142 | 14142 | 6218 | 7924 |
| orna-live | src/lib.rs | 12208 | 12208 | 3231 | 8977 |
| orna-project | src/attachments.rs | 9969 | 9969 | 9153 | 816 |
| orna-syntax | src/parser.rs | 5655 | 6475 | 147 | 6328 |
| orna-repository | src/format_context.rs | 6287 | 6287 | 4399 | 1888 |
| orna-repository | src/compact.rs | 6282 | 5921 | 1775 | 4146 |
| orna-check | src/lib.rs | 5760 | 5764 | 0 | 5764 |
| orna-storage | src/compact_parquet.rs | 5006 | 5006 | 3026 | 1980 |
| orna-runtime | src/vfs.rs | 4871 | 4871 | 2763 | 2108 |
| orna-repository | src/relation_admission.rs | 4743 | 4765 | 1906 | 2859 |
| orna-repository | src/lib.rs | 4697 | 4694 | 4424 | 270 |
| orna-project | src/object_identity.rs | 4560 | 4560 | 1919 | 2641 |
| orna-protocol | src/lib.rs | 4448 | 4460 | 2112 | 2348 |
| orna-cli | src/main.rs | 4358 | 4340 | 0 | 4340 |
| orna-compat-v1 | src/revision.rs | 4309 | 4309 | 52 | 4257 |
| orna-compat-v1 | src/revision/tests.rs | 4226 | 4226 | 0 (whole-file test) | 4226 |
| orna-compat-v1 | src/physical/tests.rs | 4145 | 4145 | 0 (whole-file test) | 4145 |
| orna-compat-v1 | src/catalogue.rs | 4124 | 4124 | 1705 | 2419 |
| orna-runtime | src/catalogue.rs | 3568 | 3568 | 1088 | 2480 |
| orna-storage | src/compact.rs | 3529 | 3529 | 1593 | 1936 |

Files over 1200 production lines: 75 of 93. Files where test lines are at least
production lines: 15 of 93.

### 2.4 Over-120-line functions

Command: the `fntest.py` analysis of the 251 recorded function entries, classed
by whether the function sits in an inline test module.

| bucket | entries | by crate |
|---|---:|---|
| test functions | 126 | runtime 50, project 29, repository 19, application 14, conformance 3, compat-v1 3, live 3, protocol 2, storage 1, sys 1, table 1 |
| production functions | 125 | compat-v1 27, runtime 19, conformance 15, repository 15, sys 15, check 10, syntax 6, application 3, value 3, cli 2, live 2, project 2, storage 2, lsp 1, project-repl 1, protocol 1, traceability 1 |

Half the function debt is test code. The production debt is spread over many
crates, so no single function split pays much; the file splits in section 4 are
what move the budget.

### 2.5 Duplicated algorithm modules

Measured with `cmp` and `diff`:

| module | copies | result |
|---|---|---|
| `unicode_16_case_properties.rs` | `orna-evaluator/src/`, `orna-exec/src/natives/` | byte-identical, 635 lines each |
| `orna-project/src/unicode16.rs` | one copy, 1,636 lines | third Unicode implementation (NFKC casefold, default ignorable) |
| `timezone.rs` | `orna-evaluator/src/`, `orna-exec/src/` | 17 changed lines |
| `decimal.rs` | `orna-value/src/` (316), `orna-exec/src/` (317) | 491 changed lines: diverged, not copies |
| `cbor` | `orna-value/src/cbor.rs` (219), `orna-repository/src/native_graph/cbor.rs` (486) | 643 changed lines in the diff: diverged, not copies |
| codec | `orna-value/src/codec.rs`, `orna-runtime/src/codec.rs` (203), evaluator `intrinsics/codecs.rs` (739) | three codec families |

`decimal.rs` diverged in two places. That is a correctness risk, not only a
size issue: two decimal implementations can round differently. The `exec` copy
is in-flight (section 6), so its deduplication is LAST.

### 2.6 Dependency seams

Fan-in is taken from section 2.1. Transitive counts come from `cargo tree -i
--edges normal`.

- `orna-value`: 22 prod dependents by the fan-in script, 28 by `cargo tree -i`
  (normal edges), 27 transitive. It is a shared-type hub, not a monolith. Its
  production code is only 82 lines in `lib.rs`.
- `orna-syntax`: 16 prod dependents, 20 transitive. `parser.rs` (6,328 prod
  lines) holds the AST types (`Declaration`, `Item`, `Visibility`, `Parameter`)
  next to the parser, so every consumer of the AST also compiles the parser.
- `orna-base`: 14 prod dependents, 28 transitive, 341 lines. Already a small
  shared crate. The split stage 7603h.3 (base-split) is the template for a
  shared crate.
- `orna-sys` `introspection.rs` (17,409 prod lines, 373 items): the only
  consumer is `orna-sys` itself (`introspection::` is referenced only inside
  `crates/orna-sys`). It is a large, self-contained plan-description module.

## 3. Ranking

Ranked by value over risk. Value = debt lines removed from the budget, plus
reuse by HIR, exec, check, repl, lsp, cli, and wasm consumers. Risk = circular
dependency, public API churn, fixture moves, and the citation and scope gates.

1. Pure shared crates with no IO: `orna-unicode`, `orna-cbor`, `orna-syntax-ast`,
   `orna-cursor`, `orna-decimal`. High reuse; low circularity risk.
2. Large production files with a single or narrow consumer: `orna-sys-plan`
   (introspection, 17,409 prod lines), `orna-live-lease`, `orna-fuse`.
   Large debt removal; low reuse.
3. Test relocation (`src/` to `tests/`): large debt reduction, not a split.
   Governed by owner decision D1 (section 5). Not a slice.
4. Repository and format splits (`orna-git-oid`, `orna-format3` readers): blocked
   behind the paused PR #9829 (section 6).
5. Evaluator, intrinsics, project-repl and application effect-handler types:
   not split. They are deleted by the HIR cutover (section 6).

## 4. Proposed new crates

Layers follow [layers.toml](layers.toml): a crate may depend only on lower
layers. Names follow the existing `orna-<name>` convention.

| new crate | layer | moves from | allowed (initial) | prod dependents |
|---|---:|---|---|---|
| `orna-unicode` | 1 | `orna-evaluator/src/unicode_16_case_properties.rs`, `orna-project/src/unicode16.rs` | none | project; exec later |
| `orna-cbor` | 1 | `orna-value/src/cbor.rs` (repository copy later) | none | value; repository later |
| `orna-cursor` | 2 | `orna-conformance/src/semantic_adapter.rs` lines 1493-1900 (ordered integer, string and decimal cursors) | `orna-value` | conformance; runtime later |
| `orna-syntax-ast` | 2 | `orna-syntax/src/parser.rs` AST types | none | syntax; check, hir later |
| `orna-decimal` | 1 | `orna-value/src/decimal.rs` plus `orna-exec/src/decimal.rs` (after reconciling 491 changed lines) | none | value; exec later |
| `orna-sys-plan` | 6 | `orna-sys/src/introspection.rs` plan-description types | `orna-sys-id` (if needed) | sys |
| `orna-live-lease` | 12 | `orna-live/src/lib.rs` lines 208-705 (supervisor and lease state machine) | `orna-storage` (verify) | live |
| `orna-fuse` | 14 | `orna-cli/src/fuse_mount/` (filesystem, 4,596 lines) | `orna-check` types, `orna-storage` (verify) | none; cli |
| `orna-vfs` | 9 | `orna-runtime/src/vfs.rs` (2,108 prod lines) | `orna-storage` | runtime |

Each row is a candidate: the allowed list is verified against the actual `use`
graph in the slice PR. A row whose allowed list cannot be satisfied without a
same-layer edge is stopped and reported, not forced.

The `layers.toml` `[[crate]]` entry, the `architecture-budget.toml` entry, and
the `ARCHITECTURE.md` section 2 table row must land in the same PR as the crate
(ARCHITECTURE.md section 6). A new crate has a production dependent in the same
PR, or it is declared `role = "tool"` (in-degree 0 is an error).

## 5. Decisions for the owner

- D1 (test relocation). Budget counts inline test code. Moving tests out of
  `src/` into `tests/` lowers the count without splitting. ARCHITECTURE.md
  requires that splits shrink debt; a relocation that only moves tests is not
  a split. This plan does not do it. If it is wanted, it is a separate, labelled
  change. Do not use `#[path]` to hide tests from the count.
- D2 (`orna-compat-v1`). 20 budget files, 51,887 lines, 0 production
  dependents, role archive. It is a quarantined archive (7603h.2). Splitting it
  is pointless: delete or keep quarantined.
- D3 (`orna-cli` `main.rs` and `fuse_mount`). These are binary code. Their split
  is for budget only, with no reuse value, unless another binary needs FUSE.

## 6. Conflicts with in-flight work

In-flight work (the notebook for this plan, verified against open PRs):

- PR #9920, `orna-exec` (csenl.27, draft): edits `orna-exec` natives, including
  the unicode copy and `decimal.rs`.
- PR #9917, `orna-hir` shadow report (draft): edits `orna-hir`.
- `orna-differential` and `orna-repl`: under active edit by other agents.
- PR #9829, reform compact model (draft, PAUSED): moves compact model into
  `orna-format3` and touches `orna-repository`.

HIR cutover deletions (`docs/design/hir-cutover.md` lines 55-69, 240-249; the
step order is the authority):

- `orna-evaluator` is deleted in step D2, after P1 and P3. Its 12,381 production
  lines are not split. The 7603h.5 eval-core-split and the 7603h.6
  stdlib-table-split are superseded by the deletion.
- `orna-intrinsics` (4,639 lines) is deleted in D3, after the evaluator.
- `orna-project-repl` is deleted with the REPL flip.
- `orna-application`'s `SourceMutationEffectHandler` (about 1,365 lines),
  `contextual_snapshot.rs` (668 lines) and `Functions`/`StandardLayer` are deleted
  with their last callers. The engine split (7603h.12) cannot start until those
  are gone.
- `orna-check/src/types.rs` is deleted in H3. The check split must wait for H3.

These splits can start immediately: section 7's slices S1 to S9 touch none of
the crates above, except as noted in each slice.

## 7. Relation to the reform epic

The owner has said `ornadb-7603h` stages `.8` (format-split), `.9` (git-split),
`.10` (state-split), `.12` (engine-split) and `.13` (fuse-split) are paused or
superseded by `ornadb-csenl`. This plan does not reopen them. It re-plans the
parts that do not conflict with HIR as the slices below, and marks the rest:

| 7603h stage | status in this plan |
|---|---|
| `.3` base-split | done (`orna-base` exists); not repeated |
| `.5` eval-core-split | superseded by HIR D2 deletion; not split |
| `.6` stdlib-table-split | superseded by HIR cutover; not split |
| `.7` sys-id-adopt | outside this plan (sys-id already exists) |
| `.8` format-split | blocked behind #9829 (L4) |
| `.9` git-split | blocked behind #9829; `orna-git-oid` (L3) |
| `.10` state-split | partly: `orna-vfs` (S8); store and publication later |
| `.12` engine-split | blocked behind HIR deletions (L6) |
| `.13` fuse-split | `orna-fuse` (S7) |

## 8. Ordered slices

Each slice is one PR for one implementer. Each slice names its new crate, the
files it moves, the callers it migrates, the gates, and its acceptance. Every
slice follows ARCHITECTURE.md section 7: `git mv`, rewrite imports, delete the
old module in the same PR, no `pub use` shim, update `layers.toml`, the budget
entry, and the ARCHITECTURE.md table in the same PR.

Acceptance block for every slice: `cargo fmt --all -- --check`,
`cargo clippy --locked --workspace --all-targets -- -D warnings`,
`cargo test --locked --workspace --all-targets`,
`python3 scripts/check_architecture.py --mode=deny`,
`python3 scripts/check_architecture.py --mode=budget`,
`cargo deny --locked check bans licenses sources`. Plus, for any change touching
the CLI or a format: byte-identical `api/sys.json`, `orna --help`, and
`orna status --porcelain`.

Startable now (independent of in-flight PRs):

- **S1 `orna-unicode`.** Move `orna-project/src/unicode16.rs` (1,636 lines) and
  the unicode case-property module (635 lines, from `orna-evaluator`, which is
  identical to the `orna-exec` copy) into layer 1. Migrate `orna-project` callers.
  The `orna-evaluator` copy is removed with the evaluator (D2); the `orna-exec`
  copy is migrated in L1 after #9920. Acceptance: `orna-project` budget and
  `unicode16.rs` no longer counted in `orna-project`; `orna-unicode` appears in
  `layers.toml` and the ARCHITECTURE.md table.
- **S2 `orna-cbor`.** Move `orna-value/src/cbor.rs` (219 lines) to layer 1. The
  repository copy (`native_graph/cbor.rs`, 486 lines) differs by 643 changed
  lines, so reconciliation is a separate prerequisite slice, not a copy. Migrate
  `orna-value` callers. Acceptance: one CBOR implementation in `orna-cbor`;
  `orna-value` loses the file.
- **S3 `orna-syntax-ast`.** Move AST types (`Declaration`, `Item`, `Visibility`,
  `Parameter`, and the AST enums) out of `orna-syntax/src/parser.rs` (6,328 prod
  lines) into layer 2. Migrate the 16 prod dependents of `orna-syntax` that use
  AST types. Coordinate with csenl.24 (H9 AST cleanup) before starting. Acceptance:
  `orna-syntax` loses AST types; `parser.rs` drops below its recorded budget or
  is recorded lower.
- **S4 `orna-sys-plan`.** Move the plan-description types from
  `orna-sys/src/introspection.rs` (17,409 prod lines, 373 items; consumer only
  `orna-sys`) into layer 6. The budget entry for `introspection.rs` shrinks from
  17,855 to the remainder. Acceptance: `introspection.rs` no longer over the
  recorded cap; the new crate file is under 1200 lines or recorded.
- **S5 `orna-fuse`.** Move `orna-cli/src/fuse_mount/` (13.2k lines, not the earlier 4,596 figure)
  into layer 14. The 7603h.13 bead is superseded by this slice. Acceptance:
  `orna-cli` budget drops by the moved lines; `orna --help` and `orna status
  --porcelain` byte-identical.
- **S6 `orna-cursor`.** Move the ordered integer, string and decimal cursor
  codecs (`orna-conformance/src/semantic_adapter.rs` lines 1493-1900) to layer 2,
  depending on `orna-value`. Conformance is `forbidden_in_shipped`; the codecs
  must not stay there. Acceptance: `semantic_adapter.rs` drops below its recorded
  budget; `orna-cursor` has a production dependent (conformance, plus runtime
  later).
- **S7 `orna-live-lease`.** Move the supervisor and lease state machine
  (`orna-live/src/lib.rs` lines 208-705) to layer 12. Acceptance: `live/lib.rs`
  budget drops by about 500 lines.
- **S8 `orna-vfs`.** Move `orna-runtime/src/vfs.rs` (2,108 prod lines) to layer 9.
  Partly the 7603h.10 state-split. Coordinate with csenl.21 (table engine),
  which touches runtime. Acceptance: `runtime` budget drops by the moved lines.
- **S9 `orna-cli` `main.rs` command modules.** Move command dispatch out of
  `orna-cli/src/main.rs` (4,340 prod lines) into command modules inside the
  crate. No new crate; intra-crate split. Acceptance: `main.rs` drops below 1200
  lines or is recorded lower; `orna --help` byte-identical.

Ordering inside the startable group: S1 and S2 first (pure, shared, reusable, no
budget risk), then S3 (largest reuse), S4 (largest debt), S5 (large debt, fuse
reuse for other binaries). S6 to S9 follow.

LAST (blocked by in-flight work or deletions):

- **L1 `orna-exec` unicode and decimal.** Migrate the `orna-exec` unicode copy to
  `orna-unicode` (after #9920 merges).
- **L2 `orna-decimal`.** Reconcile `orna-value/src/decimal.rs` and
  `orna-exec/src/decimal.rs` (491 changed lines), then move the single copy to
  layer 1 (after L1).
- **L3 `orna-git-oid`.** Move constants, `NativeOid`, `GitHashAlgorithm` from
  `orna-repository/src/native_graph.rs` (7,924 prod lines). Blocked by #9829.
- **L4 `orna-format3` readers.** 7603h.8, .16, .17. Blocked by #9829.
- **L5 `orna-check` types.** Blocked by HIR H3 deleting `types.rs`.
- **L6 `orna-application` engine split.** 7603h.12. Blocked by the HIR
  deletions in section 6.
- **L7 HIR, exec, differential, repl.** Any split of these crates waits until
  their in-flight PRs land. Not scheduled here.

## 9. Filed beads

Epic `crate-decomposition` (`ornadb-g9d7z`) holds the slices S1 to S9 and L1 to L6. Each slice is
one bead; each bead lists its acceptance. Existing 7603h beads are related, not
duplicated. The beads, their IDs, and their relations are in the bead tracker
(`bd list`), not repeated here.

## 10. Risks

- Circular dependency. Each new crate's allowed list is checked against the
  actual `use` graph; a same-layer edge needs an entry and a reviewer
  (ARCHITECTURE.md section 2).
- Public API churn. Clean cutover: callers are migrated in the same PR. No
  `pub use` shim.
- Fixtures. Fixtures must live in their consuming crate; moved tests move their
  fixtures. `scripts/fixture-manifest.sha256` pins every file under
  `crates/*/fixtures`; regenerate with `check_fixture_manifest.py --update` in the
  same PR, and do not change pinned bytes.
- Citation baseline. `crates/orna-traceability/citation-baseline.json`
  `scanned_crates` must equal the shipped-binary closure (`tests/scan_scope_closure.rs`).
  A new crate on the `orna-cli` path is added to the baseline in the same PR.
- Layer deny. `layers.toml` must list every workspace member (deny reports
  `unknown-crate:`). A new orphan (in-degree 0 without a role) is an error.
- Budget growth. A split must not raise any entry; the tolerance is
  max(5%, 25 lines), but splits are required to shrink.
- `orna-native-loader` is a recorded known orphan (V13) in the deny gate; not
  changed by this plan.
