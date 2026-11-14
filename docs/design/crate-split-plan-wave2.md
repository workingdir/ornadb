# Crate split plan, wave 2

Status: proposal. Measured on origin/main c4e637a30, after wave 1 (plan
section 8, S1-S9) and the in-flight epic children `ornadb-g9d7z.10`-`.15`.
Wave 1 is `docs/design/crate-split-plan.md`; this document does not repeat its
rows. Layers follow `docs/design/layers.toml`. The budget numbers are the
recorded values in `scripts/architecture-budget.toml`, which are what
`check_architecture --mode=budget` enforces; raw `wc -l` differs by a few
lines for some files.

## 1. Question and method

Question: which further seams, after wave 1, give the most reuse and the most
removed debt, without moving code the owner has scheduled for deletion?

Method:

1. Re-measured every crate (src and tests LOC), fan-in and fan-out from
   `cargo metadata`, the recorded budget entries grouped by crate, and the
   largest source files.
2. For each candidate seam, read the `use crate::`, `use super::*` and
   `use orna_*` edges of the moving files, so the allowed list is checked
   against the layer table before it is written.
3. Ranked by value (reuse plus debt removed) over risk. Code scheduled for
   deletion is excluded: `orna-evaluator`, `orna-compat-v1` (archive, D2),
   `orna-check` (pending the HIR cutover decisions) and the `sys.explain`
   planner. Plans that touch these are marked blocked, not scheduled.

## 2. Measurements

### 2.1 Crates (src LOC including inline tests, fan-in = dependents, fan-out = dependencies)

| crate | src LOC | prod LOC | test LOC | fan-in | fan-out |
|---|---:|---:|---:|---:|---:|
| orna-repository | 73411 | 49657 | 18012 | 13 | 6 |
| orna-compat-v1 | 60764 | 50684 | 1344 | 0 | 3 |
| orna-runtime | 60721 | 27607 | 22647 | 10 | 7 |
| orna-check | 36662 | 35924 | 27041 | 10 | 7 |
| orna-sys | 31318 | 30039 | 63764 | 7 | 6 |
| orna-evaluator | 30689 | 24660 | 52431 | 9 | 9 |
| orna-cli | 29311 | 25354 | 71533 | 0 | 22 |
| orna-application | 20728 | 8377 | 2713 | 2 | 16 |
| orna-hir | 20588 | 19926 | 2488 | 6 | 3 |
| orna-project | 20458 | 8454 | 14605 | 8 | 10 |
| orna-conformance | 19467 | 15712 | 22190 | 4 | 19 |
| orna-exec | 14994 | 14243 | 5035 | 4 | 5 |
| orna-value | 13965 | 10618 | 25454 | 27 | 2 |
| orna-live | 13337 | 8838 | 25124 | 2 | 10 |
| orna-fuse | 13186 | 13167 | 0 | 1 | 15 |

47 crates in total. `orna-value` has the highest fan-in (27): changes there
are the most expensive to make and the most valuable to make reusable.

### 2.2 Budget debt by crate (recorded files over the 1200-line cap, and the function count)

| crate | files over cap | recorded file lines | functions over 120 | function lines |
|---|---:|---:|---:|---:|
| orna-runtime | 11 | 52438 | 61 | 11906 |
| orna-compat-v1 | 20 | 51720 | 30 | 5001 |
| orna-repository | 11 | 50723 | 34 | 5002 |
| orna-sys | 5 | 25627 | 16 | 6995 |
| orna-evaluator | 4 | 21326 | 0 | 0 |
| orna-project | 4 | 18983 | 31 | 6686 |
| orna-conformance | 3 | 18722 | 18 | 3227 |
| orna-application | 3 | 17794 | 17 | 2868 |
| orna-live | 2 | 13337 | 5 | 850 |
| orna-check | 4 | 12540 | 10 | 2397 |
| orna-cli | 7 | 10821 | 2 | 392 |
| orna-storage | 2 | 8535 | 3 | 392 |
| orna-syntax | 2 | 7232 | 6 | 948 |
| orna-value | 4 | 6239 | 3 | 590 |
| orna-vfs | 1 | 4870 | 5 | 1170 |
| orna-protocol | 1 | 4448 | 3 | 419 |
| orna-lsp | 2 | 4053 | 1 | 183 |
| orna-traceability | 2 | 3712 | 1 | 122 |
| orna-stream | 1 | 2843 | 0 | 0 |
| orna-table | 1 | 2829 | 1 | 172 |

Budget totals: 92 file entries, 248 function entries. Limits: file 1200 lines,
function 120 lines.

### 2.3 Largest source files (src only)

| # | file | lines | crate | status |
|---:|---|---:|---|---|
| 1 | `orna-runtime/src/lib.rs` | 34460 | runtime | inline tests 32849 (D1) |
| 2 | `orna-sys/src/introspection.rs` | 17855 | sys | sys.explain planner (deletion candidate) |
| 3 | `orna-evaluator/src/lib.rs` | 15793 | evaluator | deletion |
| 4 | `orna-application/src/lib.rs` | 14885 | application | `.15` |
| 5 | `orna-repository/src/native_graph.rs` | 14164 | repository | `.12` (git-oid) |
| 6 | `orna-conformance/src/semantic_adapter.rs` | 13808 | conformance | W2-02 |
| 7 | `orna-live/src/lib.rs` | 11714 | live | wave 1 S7 partly done |
| 8 | `orna-project/src/attachments.rs` | 9970 | project | inline tests 9153 (D1) |
| 9 | `orna-repository/src/format_context.rs` | 6289 | repository | `.13` / PR 9829 |
| 10 | `orna-repository/src/compact.rs` | 5921 | repository | PR 9829 |
| 11 | `orna-check/src/lib.rs` | 5767 | check | `.14` / deletion |
| 12 | `orna-syntax/src/parser.rs` | 5725 | syntax | parser, no seam in wave 2 |
| 13 | `orna-storage/src/compact_parquet.rs` | 5006 | storage | W2-12 |
| 14 | `orna-vfs/src/lib.rs` | 4870 | vfs | wave 1 S8 |
| 15 | `orna-repository/src/relation_admission.rs` | 4765 | repository | no seam in wave 2 |

Further large files on the budget that are not in the top 15: `orna-fuse/src/filesystem.rs`
4597 (W2-03), `orna-sys/src/host_filesystem.rs` 2494 (W2-05),
`orna-conformance/src/semantic_adapter.rs` 13808 (W2-02),
`orna-cli/src/cli_serve.rs` 2354 (W2-11),
`orna-runtime/src/catalogue.rs` 3568 (W2-13),
`orna-traceability/src/citation_ratchet.rs` 1611 (W2-01).

### 2.4 Duplicated algorithm modules still present

- hex: `orna-repository/src/hex.rs` (40 lines), and the `orna-exec` natives hash
  module carries its own copy.
- cbor: `orna-repository/src/cbor.rs` (111 lines). The value copy already moved
  to `orna-cbor` (wave 1 S2).
- base64: `orna-base` base64 codec, `orna-exec` natives base64, and the
  `orna-evaluator` copy (deleted with that crate).
- decimal: `orna-value/src/decimal.rs` and `orna-exec/src/decimal.rs`
  (`.11`).
- unicode: wave 1 S1 moved the project copy; the `orna-exec` copy is `.10`.

## 3. Ranking

Ranked by value (reuse, debt removed) over risk. Column "blocked by" uses the
numbers in section 8.

| rank | candidate | value | risk | blocked by |
|---:|---|---|---|---|
| 1 | W2-01 citation ratchet | debt 1611 lines, no blocker | low | none |
| 2 | W2-02 semantic adapter by domain | debt 13779 lines in one file, 9 over-120 functions | medium | conformance in-flight check |
| 3 | W2-03 fuse filesystem split | debt 4596, 2 functions over 120 | low | none |
| 4 | W2-05 host filesystem | debt 2301, reuse for sys and runtime | low | none |
| 5 | W2-04 stream identity types | reuse (identity types in several crates), debt only ~250 lines | low | caller census |
| 6 | W2-10 hex/cbor dedupe | removes 151 duplicated lines | low | `.12`, PR 9829 for native_graph hex |
| 7 | W2-06 sys ABI types and provider | debt 1354, reuse across sys providers | medium | ABI type move |
| 8 | W2-07 media (mime, media) | reuse, no recorded debt | low | error-type decision |
| 9 | W2-11 serve http | debt 2354 in cli | medium | cli internals, media_range, payload policy |
| 10 | W2-12 parquet codec | debt 5006 in storage | medium | PR 9829 |
| 11 | W2-13 catalogue and schema | debt 3568 + 2118 in runtime | high | runtime-core prerequisite |
| 12 | W2-08 value schema | file 1237 over cap | medium | format3 annotation move, media move |
| 13 | W2-09 contract | reuse; no recorded debt (under cap) | medium | SchemaDescriptor, Snapshot, Value move |
| 14 | W2-14 units | reuse; no recorded debt | high | check decoupling, check deletion decision |

## 4. Proposed new crates

Layers are chosen so each crate depends only on lower layers (`layers.toml`
rule). Each new crate needs its `[[crate]]` entry in `layers.toml`, its row in
`scripts/architecture-budget.toml` where it has debt to record, and its row in
`ARCHITECTURE.md` section 2. Each new crate needs a production dependent in the
same PR, or `role = "tool"`.

| new crate | layer | moves from | allowed (initial) | prod dependents |
|---|---:|---|---|---|
| `orna-citation-ratchet` | 14 | `orna-traceability/src/citation_ratchet.rs` (1611) | none | traceability (tool) |
| `orna-stream-identity` | 2 | `orna-stream/src/lib.rs` identity types, about lines 18-263 (about 250) | `orna-value` | stream; others to census |
| `orna-host-fs` | 6 | `orna-sys/src/host_filesystem.rs` (2301 recorded, 2494 wc) | `orna-security` if needed, else none | sys |
| `orna-sys-abi` | 2 | `orna-sys/src/provider.rs` (1354) plus `AbiVersion`, `SystemEffect`, `TypedValue` | `orna-value` | sys, provider crates |
| `orna-media` | 1 | `orna-value/src/mime1.rs` (505), `orna-value/src/media.rs` (275) | none (needs error type, see W2-07) | value |
| `orna-schema` | 1 | `orna-value/src/schema.rs` (1237 recorded) | `orna-value` pieces that remain | value |
| `orna-contract` | 1 | `orna-value/src/contract.rs` (2325 wc) and `object_reference.rs` (171) | `orna-value` pieces that remain | value |
| `orna-parquet` | 10 | `orna-storage/src/compact_parquet.rs` (5006) | `orna-repository`, `orna-value`, `orna-base` | storage |
| `orna-serve-http` | 13 | `orna-cli/src/cli_serve.rs` (2354) and tests (2454) | `orna-protocol`, `orna-serving`, `orna-live` (verify) | cli |
| `orna-catalogue` | 9 | `orna-runtime/src/catalogue.rs` (3568) and `schema.rs` (2118) | `orna-runtime-core` (prerequisite), `orna-storage` | runtime |
| `orna-units` | 2 | `orna-check/src/units.rs` (1515) | `orna-value`, `orna-syntax-ast` (verify) | check, hir |

No new crate is added for the intra-crate module splits (W2-02, W2-03). Those
are module-tree changes inside the existing crate.

## 5. Decisions for the owner

1. Error type for `orna-media` (W2-07): move `orna-value`'s `Error` into a
   small `orna-value-error` at layer 0, or give `orna-media` its own error type
   with conversion at the value boundary. The plan assumes the second.
2. `orna-runtime-core` (prerequisite for W2-13): owner must confirm the name
   and whether `RuntimeState`, the codec and the error type move together.
3. `orna-object-identity` (4561 lines, `orna-project`): not scheduled. Needs an
   owner decision on layer 11 versus layer 0 before any move.
4. Whether `orna-check` survives cutover. W2-14 is blocked until the answer is
   recorded in `docs/design/hir-cutover.md`.

## 6. Conflicts with in-flight work and cutover deletions

- `orna-evaluator` (deletion): `cli_serve_app.rs` depends on `orna_evaluator`.
  W2-11 moves only `cli_serve.rs` and its tests; `cli_serve_app` stays in the
  cli crate until the evaluator is deleted. Exec-side copies (hash, base64,
  time, json) are not planned here: they wait for PR 9920 (`exec` in-flight).
- `orna-compat-v1` (archive, D2): not touched. Its 51720 recorded debt is
  deletion, not split.
- `orna-check` (pending cutover): W2-14 and any move from `orna-check` waits on
  the cutover decision. The `.14` epic child covers the types split.
- `sys.explain` planner: `orna-sys/src/introspection.rs` (17855) is not split
  in wave 2. Splitting it before the owner decides its fate would move code
  that may be deleted.
- Repository `native_graph.rs` (14164) is `.12` (git-oid). Its hex copy in
  W2-10 waits for that bead. `format_context.rs` and `compact.rs` wait for PR
  9829.
- Protocol, LSP, sys-codegen and table splits are intra-crate or oracle work.
  They are listed as deferred (section 9) and are not beads in this wave.

## 7. Relation to the reform epic

Wave 2 extends `ornadb-g9d7z`. It does not duplicate `.1`-`.15`: S1-S9 are wave
1; `.10`-`.15` are the decimal, git-oid, format3, check, application and exec
unicode items that wave 1 deferred. The beads for wave 2 are children of the
same epic, numbered after `.15`.

## 8. Ordered slices

Each slice is one PR for one implementer. Each lists files, the new crate (or
module tree), callers, gates, and acceptance, including the budget entries that
disappear. Gates for every slice: `check_architecture --mode=deny` (no new
violations), `check_architecture --mode=budget` (no new debt, the entries below
go away), `check_doc_links`, and the crate's own tests.

### W2-01 `orna-citation-ratchet`: move the citation ratchet out of traceability

- Files: `crates/orna-traceability/src/citation_ratchet.rs` (1611) becomes
  `crates/orna-citation-ratchet/src/lib.rs`.
- Callers: `orna-traceability/src/main.rs` (lines 60, 73, 115),
  `orna-traceability/src/lib.rs` (`pub mod citation_ratchet`, removed),
  `orna-traceability/tests/scan_scope_closure.rs` (line 3). Migrate all three.
  No `pub use` re-export in traceability (clean cutover).
- New crate: `layer = 14`, `role = "tool"`, `allowed = []`. The crate depends
  on std and serde only, which the verified `use` graph confirms.
- Gates: deny and budget as above; `cargo test -p orna-citation-ratchet -p orna-traceability`.
- Acceptance: budget entry `citation_ratchet.rs = 1611` under orna-traceability
  removed; traceability crate recorded total drops from 3712 to about 2101.

### W2-02 `orna-conformance`: split `semantic_adapter.rs` by domain (module tree, no new crate)

- Files: `crates/orna-conformance/src/semantic_adapter.rs` (13808 wc, 13779
  recorded) becomes `semantic_adapter/` with one module per domain (admission,
  transactions, relation operators, continuation, faults). Domain boundaries are
  taken from the existing section comments; the cursor codecs are already in
  `orna-cursor` (wave 1 S6) and are not moved again.
- Callers: none outside the module (`pub` surface is re-exported by
  `semantic_adapter/mod.rs` only inside the crate, no new public API).
- Gates: deny and budget; `cargo test -p orna-conformance`.
- Acceptance: the file entry `src/semantic_adapter.rs` is removed or lowered;
  function entries `handle_inner_with_budget` (467), `relation_aggregate` (132),
  `relation_quantifier` (188), `admit_transaction_project_with` (203),
  `evaluate_module_assertion_continuation` (233) and the other
  `semantic_adapter.rs::` function entries are lowered below 120 lines by the
  split, or split into functions of at most 120 lines. Each function entry that
  falls below the cap is deleted by `--update`.
- Risk: medium. Check the open PR list for `semantic_adapter.rs` before starting.

### W2-03 `orna-fuse`: split `filesystem.rs` into a module tree (no new crate)

- Files: `crates/orna-fuse/src/filesystem.rs` (4597) becomes `filesystem/`
  modules by FUSE operation group (lookup and attributes, file handles, create
  and write, commit and blob candidates, directory listing).
- Callers: `orna-fuse` internal only; `orna-cli` uses the crate's public API,
  which does not change.
- Gates: deny and budget; `cargo test -p orna-fuse`, `cargo test -p orna-cli`
  (the mount integration).
- Acceptance: file entry `src/filesystem.rs` (4596) removed; function entries
  `filesystem.rs::commit_blob_candidate` (169) and `filesystem.rs::create` (139)
  are split below 120 lines or moved into smaller functions.

### W2-04 `orna-stream-identity`: move stream identity types out of `orna-stream`

- Files: `crates/orna-stream/src/lib.rs` identity region (about lines 18-263:
  `Component`, `ConsumerIdentity`, `DeliveryIdentity`, `IdentityError`,
  `Position`, checkpoint merge types) becomes
  `crates/orna-stream-identity/src/lib.rs`.
- New crate: `layer = 2`, `allowed = ["orna-value"]`. The verified use graph of
  the identity region is `orna_value` only.
- Callers: `orna-stream` and every crate that imports the identity types. The
  caller census is the first step of the PR; migrate imports, add no re-export.
- Gates: deny and budget; `cargo test -p orna-stream -p orna-stream-identity`.
- Acceptance: `orna-stream` recorded total (2843) drops by about 250; the identity
  types are reusable by sibling crates without depending on stream checkpoint code.
- Note: debt removed is small. This slice is ranked for reuse, not debt.

### W2-05 `orna-host-fs`: move the host filesystem adapter out of `orna-sys`

- Files: `crates/orna-sys/src/host_filesystem.rs` (2494 wc, 2301 recorded)
  becomes `crates/orna-host-fs/src/lib.rs`.
- New crate: `layer = 6`, `allowed = ["orna-security"]` if the use graph needs
  it, otherwise `allowed = []`. The verified use graph has no `crate::` uses; only
  the `sys_host_operation` macro, which moves with the file or is exported from
  `orna-sys` (decided in the PR, not both).
- Callers: `orna-sys` provider registration only. Migrate; no re-export.
- Gates: deny and budget; `cargo test -p orna-sys -p orna-host-fs`.
- Acceptance: budget entry `src/host_filesystem.rs = 2301` under orna-sys
  removed; sys recorded total drops from 25627 by about 2300.

### W2-06 `orna-sys-abi`: move the provider ABI types and provider module out of `orna-sys`

- Files: `crates/orna-sys/src/provider.rs` (1384 wc, 1354 recorded) plus the
  `AbiVersion`, `SystemEffect` and `TypedValue` definitions that `provider.rs`
  imports via `crate::`.
- New crate: `layer = 2`, `allowed = ["orna-value"]`. Must be below `orna-sys`
  (layer 6). The PR verifies the three types' own imports before the layer is
  final.
- Callers: `orna-sys` and provider crates. Migrate imports.
- Gates: deny and budget; `cargo test -p orna-sys`.
- Acceptance: budget entry `src/provider.rs = 1354` under orna-sys removed.
- Risk: medium. The ABI types are wire-visible; check the snapshot fixtures
  (`system_provider_abi.rs`) before and after.

### W2-07 `orna-media`: move MIME and media types out of `orna-value`

- Files: `crates/orna-value/src/mime1.rs` (505), `crates/orna-value/src/media.rs` (275).
- New crate: `layer = 1`, `allowed = []`, subject to decision 1 in section 5
  (error type). Until that decision, the slice may not start.
- Callers: `orna-value` (`lib.rs` and tests) and every crate that imports media
  types. Migrate; no re-export.
- Gates: deny and budget; `cargo test -p orna-value -p orna-media`.
- Acceptance: `orna-value` recorded total (6239) and fan-in-heavy surface drop
  by about 780; media becomes reusable at layer 1.

### W2-08 `orna-schema`: move value schema out of `orna-value` (blocked)

- Files: `crates/orna-value/src/schema.rs` (1237 recorded, over the 1200 cap).
- New crate: `layer = 1`. Blocked: `schema.rs` calls format3's
  `validate_annotation_fields`, and media (W2-07). Format3's owner is `.13` (L4).
  Do not start until `.13` lands the format3 annotation move or W2-07 lands the
  media move and the annotation function is reachable at layer 1.
- Acceptance: budget entry `src/schema.rs = 1237` under orna-value removed, and
  the schema functions `validate_value_as_type` (128) stay under 120 lines after
  the move.

### W2-09 `orna-contract`: move contract types out of `orna-value` (blocked)

- Files: `crates/orna-value/src/contract.rs` (2325 wc, under the file cap by the
  recorded measure, no budget entry) and `object_reference.rs` (171).
- New crate: `layer = 1`. Blocked: `contract.rs` imports `crate::{Error as ValueError,
  Raw as OvbRaw, SchemaDescriptor, Snapshot, Value, canonical_uuid_text}`.
  `SchemaDescriptor`, `Snapshot` and `Value` stay in `orna-value` (they are the
  core types). The contract crate therefore depends on `orna-value`, which makes
  it layer 2, not 1, unless the core types move to layer 0 first. Decide in the PR.
- Acceptance: contract has no recorded debt; the slice is for reuse. It does not
  remove a budget entry.

### W2-10 `orna-cbor` and `orna-base`: remove the repository hex and cbor copies

- Files: `crates/orna-repository/src/hex.rs` (40) and
  `crates/orna-repository/src/cbor.rs` (111). The hex functions move to
  `orna-base` (layer 0). The cbor functions are compared with `orna-cbor`
  (wave 1 S2); if they are byte-identical in behaviour, the repository copy is
  deleted and callers import `orna-cbor`.
- Callers: `orna-repository` internals, and `native_graph.rs` hex calls, which
  wait for `.12`.
- Gates: deny and budget; `cargo test -p orna-repository -p orna-cbor -p orna-base`.
- Acceptance: 151 duplicated lines removed. If cbor differs in behaviour, the
  slice stops and reports the difference; it does not force a merge.

### W2-11 `orna-serve-http`: move the serve HTTP surface out of `orna-cli` (blocked)

- Files: `crates/orna-cli/src/cli_serve.rs` (2354) and its tests (2454 lines).
- New crate: `layer = 13`. Blocked: `cli_serve.rs` uses `super::*` (cli
  internals), `media_range` and `apply_payload_policy`. Those move to the new crate
  (or to a layer-13 module) first. `cli_serve_app.rs` stays in cli because it
  depends on `orna_evaluator`, which is scheduled for deletion.
- Callers: `orna-cli` command wiring. Migrate imports.
- Acceptance: budget entry `src/cli_serve.rs = 2354` under orna-cli removed;
  cli recorded total (10821) drops by about 2354.

### W2-12 `orna-parquet`: move the parquet physical codec out of `orna-storage` (blocked)

- Files: `crates/orna-storage/src/compact_parquet.rs` (5006), physical codec part.
- New crate: `layer = 10`. Blocked by PR 9829's decision on the storage/repository
  compact boundary. Uses `orna_repository` (layer 8, lower, allowed), `crate::compact`
  (must move with it or stay behind a trait), `SchemaDescriptor` (value) and
  `orna_base::hex`.
- Acceptance: budget entry `src/compact_parquet.rs = 5006` removed; the function
  entry `decode_verified_bytes_for_role` (123) is split below 120 lines or moved;
  storage recorded total (8535) drops by about 5000.

### W2-13 `orna-catalogue`: move catalogue and schema out of `orna-runtime` (blocked)

- Files: `crates/orna-runtime/src/catalogue.rs` (3568), `crates/orna-runtime/src/schema.rs` (2118).
- New crate: `layer = 9`. Blocked: both use `crate::RuntimeState`, the codec and the
  error type, and `schema.rs` uses `crate::stream_admin` and `observation`. A
  runtime-core extraction must land first (decision 2 in section 5).
- Acceptance: budget entries `src/catalogue.rs = 3568` and `src/schema.rs = 2118`
  under orna-runtime removed; runtime recorded total (52438) drops by about 5686.
  Function entries under `catalogue.rs` and `schema.rs::migrate_*` move with the code.

### W2-14 `orna-units`: move unit arithmetic out of `orna-check` (blocked)

- Files: `crates/orna-check/src/units.rs` (1515 wc). Not on the budget (under the
  cap by the recorded measure).
- New crate: `layer = 2`, `allowed = ["orna-value", "orna-syntax-ast"]` (verify).
  Blocked: `units.rs` uses `super::*` (check internals). Decouple check first
  (`.14`, H3). Also blocked by the `orna-check` deletion decision (section 6);
  do not move code out of a crate that may be deleted.
- Acceptance: reuse; no recorded debt. Note: `hir` lowering units (269 lines) stays.

## 9. Filed beads

Each slice is one bead under epic `ornadb-g9d7z`. Startable ones have no open
blocker in this document. Beads are filed with `bd create` from the main
checkout after this document merges; their IDs are listed in the PR body.

Startable now (first five): W2-01, W2-02, W2-03, W2-04, W2-05.
Blocked: W2-06 (ABI move), W2-07 (decision 1), W2-08 (`.13`), W2-09 (core
types), W2-10 (`.12`, PR 9829 for native hex), W2-11 (cli internals, evaluator
deletion), W2-12 (PR 9829), W2-13 (runtime-core), W2-14 (check decouple,
cutover decision).

Deferred, no bead in this wave (covered elsewhere or blocked on in-flight work):

- exec decimal: `.11` (existing).
- exec time, calendar, timezone: `orna-time` (layer 1), after PR 9920.
- exec json parse and number: `orna-json` (layer 1), after PR 9920.
- exec hash and base64: after PR 9920.
- protocol patch module split: intra-crate, no crate change; planned later.
- lsp analysis and server split: tool, intra-crate; planned later.
- sys-codegen `build_support` split: tool, planned later.
- table (`orna-table`, oracle, layer 14): H6.
- repl (`orna-repl`, tool, layer 5): in-flight.
- `object_identity.rs` (4561, `orna-project`): decision 3.
- inline test modules on `attachments.rs`, `orna-runtime/src/lib.rs`,
  `orna-value/src/lib.rs`: D1 (move tests out, no code change).
- `native_graph.rs`: `.12`.
- `format_context.rs` and `compact.rs` (repository): PR 9829.
- `orna-application` engine split: `.15`.

## 10. Risks

- Budget drift: recorded values in `architecture-budget.toml` differ slightly from
  `wc -l`. Acceptance uses the recorded values; `--update` lowers them. A slice
  that claims a number must check the budget output, not `wc`.
- Blocked slices can be started early by mistake. Each blocked slice names its
  blocker in its acceptance; the implementer confirms the blocker is merged first.
- W2-02 and W2-03 are module-tree changes with large diffs. They must not be
  mixed with behaviour changes; reviewers should check for moved-only hunks.
- W2-05 and W2-06 touch sys, which has in-flight snapshot work (PR 9935 merged).
  Check open PRs for `orna-sys` before starting.
- Deletion of `orna-evaluator`, `orna-compat-v1` or `orna-check` can make a
  planned move moot. Re-check section 6 before each slice starts.
- The `layers.toml` edits for new crates must land with the crate (ARCHITECTURE
  section 6). A slice that adds a crate without a production dependent fails
  check_architecture unless `role = "tool"`.
