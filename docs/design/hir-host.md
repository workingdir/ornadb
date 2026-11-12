# HIR host and Money design (stage H5 and the H4 money rules)

Status: design, not implemented. Bead `ornadb-csenl.20` (H5: Host trait, natives, link) and the H4 money rules of
`ornadb-csenl.19` / `.17`. Written by agent `HirHostDesign` against `origin/main` `30ef8fbe8`. The slice beads are
filed under epic `ornadb-csenl`; their ids are in section 3.

Two questions block executor agreement (`exec-share.toml`: `agreed = 48` of `evaluable = 110`):

1. **Part A.** `orna-exec` has no `Host` trait, so a `sys.*` read has nowhere to get data.
2. **Part B.** `Money<C>` is not declared anywhere in HIR, so `Money<GBP>`, `12.34.GBP`, `E5001`, `E5002` and `E5003`
   cannot be typed or run.

[hir.md](hir.md) section 4.4 and section 5.2 sketch the `Host` and `TableStore` traits;
[kernel-and-library.md](kernel-and-library.md) sections 5.7 and 7.4 decide that `Money<C>` is a library type and
sketch it; [sys-as-native.md](sys-as-native.md) section 3.5 decides how hosts register `sys` members. This document
does not reopen those decisions. It turns them into slices small enough for one implementer each.

Numbers below come from running `cargo test -p orna-differential --test exec_share` with `EXEC_SHARE_IDS=1` and a
scratch probe over the named cases at `30ef8fbe8`, and from reading the named files. Anything not measured is marked
`[INFERENCE]`.

## 0. Measured state, and two corrections to the brief

### 0.1 What exists

| Piece | State | Where |
|---|---|---|
| `TableStore` trait, `MemoryStore`, store contract tests | **exists** (hir.md "H6" is partly done) | `crates/orna-exec/src/store.rs`, `store/memory.rs`, `store/contract.rs` |
| `Host` trait, `HostManifest` | **absent** | hir.md 4.4 only |
| `Machine` | owns `store: Box<dyn TableStore>`; no clock, no entropy, no snapshot, no sys state | `crates/orna-exec/src/machine.rs` |
| Entry points | `run_entry`, `measure_entry`, `measure_entry_with_store`, `measure_entry_in`, each builds a `Machine` | `machine.rs` |
| `Callee::Sys { def, targs }` | executed exactly like `Callee::Native`: registry lookup by `(module, name)`; only `sys.blob_*` are registered | `eval.rs:237`, `natives/mod.rs` |
| Relation pipeline | `Source::{Table(DefId), Rows(list)}`; stages `Filter/Map/Take/Drop`; terminals `count/first/one/every/exists`; other `RelOp` is `NotYet (H7)` | `relation/pipeline.rs` |
| `TableOp::AsOf` | `Stop::NotYet("as_of needs the activation snapshot (H5)")` | `table.rs:68` |
| `SysAdmission` (lowering-time view of linked `sys` members) | in PR #9918 (open): `orna-hir/src/lower/admission.rs`; `CatalogueAdmission` in `orna-check` reads `orna_sys::system_provider_linkage()` | PR #9918 |
| Production provider linkage | 15 callables, 3 relations, 2 grouped handles linked; everything else `None` | `orna-sys-id/build/generator.rs:4482-4520`, `orna-sys/src/provider_linkage.rs` |
| `Value::Money { amount, currency }` | decode and encode only; no construction, no arithmetic | `orna-exec/src/value.rs:62`, `raw_value.rs:130` |
| `Money`, `Currency`, `Rate` declarations | **none** in `crates/orna-hir/interface` | `core/` has `temperature.orna`, `units.orna`, `range.orna`, `display.orna`, `ops.orna` |

The linked set is: callables `sys.blob.{length,digest,verify,read,to_bytes,from_bytes,annotate,media_type,suffix,
same_content}` (Evaluator), `sys.blob.capture_file` (HostOperationRegistry), `sys.explain(Query)`, `sys.rt.info`
(Evaluator), `sys.admin.{pause,resume}_stream` (LiveAdministration); relations `sys.Materialization` (Evaluator),
`sys.MaintenanceJob`, `sys.Storage` (PublicationMetadata); handles `sys.catalog.{columns,tables}` (Evaluator).
`sys.rt.runs`, `sys.rt.checkpoints`, `sys.catalog.objects`, `sys.git.commits`, `sys.storage.objects` and every
`as_of` view are **catalogued but unlinked** in production.

### 0.2 Correction 1: the sys cases are mostly not Host cases

The brief says the `sys.*` cases "hit NotYet 'value use of a definition'". Measured, one case does
(`invalid/relation-equality.orna`, and it is not a `sys` case). Of the 48 evaluable cases the executor does not run
(measured breakdown: 14 bind rows/arguments, 22 lower with a diagnostic, 3 `relation operator (H7)`, 2 `money or type
constructor`, 1 each `as_of`, `engine native not ported`, `field access ... (H2)`, `intrinsic form`, `member call (H2)`,
`value use of a definition`, `no entry function`), the `sys` ones are:

| Case | Old pipeline outcome | HIR today (lowering diagnostics) | What closes it |
|---|---|---|---|
| `valid/sys-checkpoint` (`sys.Checkpoint.as_of(HEAD)`) | `sys.version.incompatible` | `` `sys` is a module, not a value `` | slice A1 (lowering of `sys` value paths) + admission |
| `valid/sys-run-history` (`sys.Run.as_of(HEAD) \| sort_by(..)`) | `sys.version.incompatible` | same | A1 |
| `valid/sys-table-query` (`sys.catalog.objects \| filter(.. sys.ObjectKind.table ..)`) | `sys.version.incompatible` | same, twice | A1 |
| `valid/historical-query` (`contacts.Contact.as_of(sys.snapshot("HEAD~3"))`) | `sys.version.incompatible` | `the sys declarations are not represented yet` | A1 (PR #9918 already rewrites this diagnostic for an unlinked member) |
| `valid/historical-program` | `sys.version.incompatible` | four diagnostics, one is `energy.daily` unresolved | A1, plus an `energy.daily` fixture function (harness, not Host) |
| `valid/presentation-watch` (`std.ui.Table(sys.rt.streams)`) | `sys.version.incompatible` | `Page` not exported, `sys` not a value | A1, plus `std.ui` (separate native family) |
| `valid/sys-storage` (`sys.Storage \| filter(..)`) | `ORNA-EVAL-UNSUPPORTED` | `sys` not a value | A1, then A3 (linked relation with no data) |
| `valid/cwd-head` (`contacts.Contact.as_of(CWD) \| count`) | `ORNA-EVAL-NAME` (harness has no store) | runs, stops at `as_of` | A5, as a `[[harness-allowance]]` |
| `invalid/legacy-sys-runtime`, `legacy-sys-storage-call`, `mutate-sys-commit` | checker codes (`ORNA100-E-SYS-RUNTIME`, `ORNA100-E-SYS-STORAGE-CALL`, `E7001`) | `ORNA-S022-UNSUPPORTED` | H3 checker rules, not this document |

So the first blocker is **lowering**: `lower/namespace.rs` refuses to treat `sys.<name>` as a module export
(`namespace_member` and `missing_module_export` both return `None` for a `ModuleOrigin::Sys` module), the field
lowering then lowers `sys` as a bare name, and `lower_name` reports "is a module, not a value". PR #9918 makes the
*call* path and the `base.name` field path consult `SysAdmission`; it does not give a linked `sys` member a value.

Consequence for the ratchet: in this corpus the old pipeline never produces a `sys` row. It rejects every unlinked
member before evaluation and answers `UNSUPPORTED` for the one linked relation it has no data for. Provider **data**
therefore cannot raise `agreed`; admission and a correct "linked but empty" answer can. Provider data is needed by
the production consumers (C3 `orna run`, C4 REPL, C5 `serve`), and the minimal provider of section A5 serves them.

### 0.3 Correction 2: Money is not partly done

`Value::Money` exists for the wire form only. There is no `Money` type in `Program.tys`, no instance, no suffix
lowering for `.GBP`, no `Callee::Ctor` for `Money<C>(x)`, and no `E5xxx` code in `orna-hir` (the old codes live in
`orna-check/src/diagnostics.rs:65-70`: `E5001` time plus energy / incompatible dimensions, `E5002` affine temperature
add, `E5003` absolute quantity sum and currency mismatch, `E5004` `Money<C>(Float)`, `E5103` Money with `Float`
operand). `diagnostic_codes_differing.txt` records `E5001`, `E5002`, `E5003` and `E5004` as `=> (none)` or
`ORNA-S012-UNRESOLVED` for six cases. The environment ratchet puts `Money`, `GBP`, `EUR`, `USD` in
`CURRENCY_AND_UNIT` (`orna-differential/src/environment.rs:98`).

--------------------------------------------------------------------------

## 1. Part A: the Host trait

Sections A1 to A6 below are filled in by the following commits; see section 3 for the slice list.
