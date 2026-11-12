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

### A1. What the `Host` owns

One trait, defined in `orna-exec`, replaces the three effect-handler traits of the old evaluator (hir.md 4.4). It owns
exactly what a run needs from outside the program, and nothing the program can compute:

| Concern | Today in `orna-exec` | Under `Host` |
|---|---|---|
| Tables | `Machine.store: Box<dyn TableStore>` | `Host::store()`; `TableStore` is unchanged |
| Clock | `core.now` is declared (`effect clock`), not bound | `Host::now()`, fixed for the activation (ORNA-TXN-005) |
| Entropy | `core.uuid7` declared (`effect randomness`), not bound | `Host::entropy(&mut [u8])` |
| Activation snapshots | none; `TableOp::AsOf` is `NotYet` | `Host::activation()`: the `cwd` and `head` snapshots, `HEAD` and `CWD` read it |
| Historical reads | none | `Host::store_at(SnapshotId)`: a read-only `TableStore` |
| `sys` callables | `Callee::Sys` runs registry natives; only `sys.blob_*` exist | `Host::sys_call(slot, args)` for any member the registry does not implement |
| `sys` relations and statics | none | `Host::sys_relation(slot, at)` and `Host::sys_static(slot)` |
| Filesystem, network, process | not in `orna-exec` | `Host::io() -> Option<&mut dyn HostIo>`; **out of scope here**, stays `None` |
| `parallel` children | not executed | `Host::fork`; **out of scope here** (hir.md 4.4) |

```rust
// orna-exec/src/host.rs
pub trait Host {
    fn manifest(&self) -> &HostManifest;                 // which sys members this host serves
    fn store(&mut self) -> &mut dyn TableStore;
    fn store_at(&mut self, at: SnapshotId) -> Result<Box<dyn TableStore + '_>, HostFailure>;
    fn now(&self) -> HostInstant;                        // unix seconds + nanosecond, fixed per activation
    fn entropy(&mut self, out: &mut [u8]) -> Result<(), HostFailure>;
    fn activation(&self) -> &ActivationContext;          // cwd: SnapshotId, head: Option<SnapshotId>
    fn sys_call(&mut self, slot: SysSlot, args: Vec<Raw>) -> Result<Raw, HostFailure>;
    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure>;
    fn sys_relation(&mut self, slot: SysSlot, at: Option<SnapshotId>)
        -> Result<Box<dyn RowSource + '_>, HostFailure>;
}
pub trait RowSource { fn next(&mut self) -> Result<Option<Raw>, HostFailure>; }   // canonical record rows
pub struct HostFailure { pub code: &'static str }        // a canonical code; the machine maps it to `Stop::Error`
```

Design choices, each with its reason:

- **Values cross as `orna_value::Raw`, not `orna_exec::Value`.** Table rows already cross as canonical bytes decoded
  by `raw_value::from_raw`; provider rows take the same path, so a provider in a higher layer needs only `orna-value`
  (layer 1) and the row type declared in `sys.orna`. `Value` stays private to the executor. `from_raw` rejects shapes
  it does not read back (`Instant`, `Duration`, generic enums, `RowRef<T>` are `NotYet (H6)` today); each provider slice
  adds the shapes its row type needs, with a test, and no more.
- **`SysSlot`, not a name, on the hot path.** A host registers qualified names once
  (`manifest.register("sys.Storage") -> SysSlot`), as sys-as-native.md 3.5 rule 2 decides. The machine binds each
  `sys` definition of the program to a slot by qualified name when it starts, as `link::bind` already does for
  natives. A call or read then carries the slot. A program that names a member the host did not register cannot have
  been admitted (A3); if it reaches the machine anyway the answer is `ORNA-EVAL-UNSUPPORTED`, never a panic.
- **No `&Expr` and no syntax.** A host receives a slot and canonical arguments, as hir.md 4.4 requires.
- **`Machine` holds `Box<dyn Host>`.** `machine.store` becomes `machine.host.store()`. The existing entry points
  (`run_entry`, `measure_entry`, `measure_entry_with_store`, `measure_entry_in`) keep their signatures and build a
  `StoreHost` (a `MemoryStore`, `now` fixed at the epoch, no entropy, no sys members). They are not shims: they are
  the entry points that need no host data. New `run_entry_with_host` and `measure_entry_with_host` take one.

### A2. Layering

`docs/design/layers.toml` puts `orna-exec` at layer 4, `orna-hir` and `orna-sys-id` at 3, `orna-sys` at 6,
`orna-runtime` at 9, `orna-application` at 13 and `orna-differential` at 14.

- **`Host`, `HostManifest`, `SysSlot`, `RowSource`, `HostFailure`, `StoreHost`: `orna-exec`** (new `src/host.rs`).
  hir.md 2.1 mentions a separate `orna-host` crate; hir.md 10.1 says `orna-exec` defines `Host` and `TableStore`.
  This document settles it: **`orna-exec`, no `orna-host` crate.** `TableStore` is already there, a new crate would
  hold only a trait that cannot be used without `orna-exec`'s `Value`-free types anyway, and a layer-4 trait is
  implementable from every higher layer.
- **Implementations live above.** `orna-sys` (6) may implement `sys_call` for the host operations it owns;
  `orna-runtime` (9) may implement `sys_relation` over its observation projections; `orna-application` (13) composes
  them. Each edge is a downward dependency on `orna-exec`, which `layers.toml` allows without an entry. The cost is
  that `orna-exec`'s dependency closure (`num-bigint`, `sha2`, ...) enters those crates; none is new to the shipped
  binary.
- **Admission stays in `orna-hir`.** `SysAdmission` is a lowering-time trait at layer 3 so `orna-check` (layer 4, no
  dependency on `orna-exec`) can use it. `HostManifest` implements `SysAdmission` (`status(path)` is `Linked` when the
  path's qualified name is registered, else `Unlinked`), so a host that has a manifest has its admission. Until the
  production hosts register manifests, `CatalogueAdmission` (orna-check, over `orna_sys::system_provider_linkage()`)
  remains the production admission, and a parity test (HA3) asserts that `HostManifest` of the production host equals
  that linkage, so the two cannot drift. This is the "pinned list per host" ratchet of sys-as-native.md 3.5 rule 3.
- **`layers.toml`: no change is needed for the trait.** Edges added by the slices: `orna-runtime -> orna-exec` (9 to
  4, HA8) and `orna-differential -> orna-exec` (already present). `orna-check` must **not** gain `orna-exec`
  (same layer, would need an `allowed` entry); it does not need one. If a later stage wants the checker to read a
  host's manifest directly, pass a `SharedAdmission` built by the caller at layer 13; do not add the edge.

### A3. Admission and the host agree by construction

Two checks must never disagree: lowering (`SysAdmission`, rejects an unlinked member with `sys.version.incompatible`,
ORNA-SYS-008, before any write) and the machine (serves a linked member). Rule: **lowering admits a member iff
`HostManifest` registers it; the machine serves a member iff it is registered.** A registered member whose host has no
data for it answers `ORNA-EVAL-UNSUPPORTED` (the code the old evaluator reports for `valid/sys-storage`, measured in
0.2). Every unregistered member is rejected before evaluation, which is the old behaviour for the other six `sys`
cases of 0.2.

### A4. How provider data plugs into relations and pull trees

hir.md 5.4 defines a relation as a pull tree over cursors. The executor's version today is `Pipeline { source, stages }`
(`relation/pipeline.rs`) with `Source::{Table, Rows}`. A provider relation is one more source, and nothing else in the
pipeline changes.

```rust
// value.rs: a relation that is not yet read. No canonical form (like a closure): `to_raw` is ORNA-EVAL-VALUE.
Value::Rel(Rc<RelHandle>)
pub enum RelHandle {
    Sys   { slot: SysSlot, at: Option<SnapshotId> },       // sys.Run, sys.catalog.tables, sys.rt.runs, `.as_of(s)`
    Table { table: TableId, at: Option<SnapshotId> },      // `Table.as_of(s)`
}
// relation/pipeline.rs
enum Source { Table(DefId), Rows(Rc<[Value]>), Handle(Rc<RelHandle>) }
```

- `source_of` turns a `Value::Rel` (the value of `sys.Run`, of `sys.rt.runs`, of `Table.as_of(s)`) into
  `Source::Handle`. `drive` opens `host.sys_relation(slot, at)` (or `host.store_at(at)` and a scan for `Table`) and
  pushes one decoded row at a time into the same `push`/`Sink` machinery, charging one step per row exactly as
  `walk_table` does. `take(n)`, `first`, `exists` stop the source, so a provider is never read past what a terminal
  needs, and the cursor (the `RowSource`) is dropped on every path.
- Rows are `Raw::Map` records, decoded by `raw_value::record(from_raw(..))`: the same function table rows use, so a
  `sys` row and a table row are the same `Value::Record` downstream, and `filter(object => object.kind == ...)` needs
  nothing new.
- `sys.rt` and `sys.database` are `native static`s whose value is a record containing `Relation<..>` fields
  (`RuntimeView.runs`). `sys_static(slot)` returns the record with each relation field as a **handle**, not rows: the
  field is a `Raw` placeholder that the machine turns into `Value::Rel(RelHandle::Sys { slot, .. })`, where the slot is
  the relation's own registered name (`sys.rt.runs`). A host that never registered `sys.rt.runs` makes the whole
  `sys.rt.runs` path unlinked at admission.
- `as_of`: `Value::Rel(Sys{slot, None}).as_of(s)` yields `Sys{slot, Some(s)}`. A `sys.rt.*` handle refuses `as_of`
  with the code the spec directs (ORNA-SYS-013, a checker rule; the machine answers `ORNA-EVAL-UNSUPPORTED` if it is
  reached). A historical read never falls back to the current snapshot (ORNA-SYS-011): `store_at`/`sys_relation` with
  an unknown snapshot returns `sys.snapshot.not_found`.
- Blocking operators (`sort_by`, `group_by`, `distinct`, `union`) are stage H7 (`NotYet`) and are not part of this
  design; `valid/sys-run-history` is rejected by admission before it would need `sort_by`.

### A5. The minimal first provider, and what data it needs

The corpus needs no provider data (0.2). The consumers do: `orna run`, the REPL and `serve` read `sys.catalog.*` and
`sys.rt.*`. The smallest provider that is real, exercised, and has no external dependency serves exactly the production
linkage that the **evaluator** owns (`LINKED_*` in `orna-sys-id/build/generator.rs`, host kind `Evaluator`):

| Member | Data needed | Source |
|---|---|---|
| `sys.catalog.tables` (and canonical `sys.Table`) | one `sys.Table` row per `DefKind::Table`: `reference`, `object`, `definition`, `key`, `storage`, `row_count` (`null`, `row_count_exact = false`); `columns`, `assertions`, `references` are relation handles | the lowered `Program` (`TableDef`), as sys-as-native.md 3.6 maps `sys.Function` columns to `def.rs` |
| `sys.catalog.columns` (and `sys.Column`) | one row per table field: `name`, `position`, `type`, `optional`, `key_position`, `default_expression`, `computed_expression` | `FieldDef` and `FieldInitializer` of the same program |
| `sys.rt.info` | `sys.RuntimeInfo`: versions, build id, mode, `read_only`, `supported_profiles` | a `HostInfo` record the embedder supplies |

This is `ProgramCatalog` in `orna-exec/src/host/catalog.rs` (slice HA6). It needs no storage and no snapshot, and the
`Reference`/`Object` ids are derived from the same `ObjectId` scheme `Value::Enum` already uses (`variant_ids`).
`sys.Materialization`, `sys.MaintenanceJob` and `sys.Storage` need publication metadata (`orna-runtime`) and are not
in the first provider; with no data they answer `ORNA-EVAL-UNSUPPORTED`, which is what the old evaluator answers for
`valid/sys-storage`. `sys.rt.runs` and `sys.rt.streams` come from `orna-runtime`'s `SysCurrentRuntimeProjection`
(`orna-runtime/src/observation.rs:79`) in slice HA8; that projection already states which fields the runtime does not
own (`FunctionRef`, `ObjectRef`, `InvocationRef`), and HA8 leaves those fields `null` as the projection does.

### A6. The test provider for the differential harness

`orna-exec` (feature `test-support`) gets `FixtureHost`: a `StoreHost` plus a map `qualified name -> Vec<Raw>` of rows,
a fixed `ActivationContext`, a fixed clock and a counter entropy. Tests in `orna-exec` and the harness build it with
the rows they need, so no test depends on `orna-runtime` or a repository. The harness (`orna-differential`
`observe_exec`) keeps running the corpus on `StoreHost`: the old observer supplies no sys data either (0.2), so
`agreed` cannot depend on rows. `FixtureHost` is also what the REPL differential (H8) uses for `sys.rt.info`.

`valid/cwd-head` is the one corpus case that needs an activation snapshot. The old observer answers `ORNA-EVAL-NAME`
because it has no store, so the executor, once it runs `as_of`, returns the spec result (`{ cwd: 0, head: 0 }`) and
the case becomes a `[[harness-allowance]]` against `ornadb-csenl.21`, not an agreement. `HEAD` and `CWD` are not
declared anywhere in HIR today (the environment ratchet lists them under `CONNECTOR_AND_HOST`). Slice HA5 declares them
as `native static HEAD`/`CWD` of type `sys.SnapshotRef` in the root environment. `[INFERENCE]` the spec text names
`HEAD` as a root name (ORNA-STATE-003 `relation.as_of(HEAD)`) but I did not find the chapter that declares it; if the
owner declares them as `sys.database.head`/`.cwd` aliases instead, only that one declaration changes.

--------------------------------------------------------------------------

## 2. Part B: Money

Section 2 is filled in by the following commits.

--------------------------------------------------------------------------

## 3. Slices and beads

Section 3 is filled in by the following commits.
