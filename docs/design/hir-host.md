# HIR host and Money design (stage H5 and the H4 money rules)

Status: design, not implemented. Bead `ornadb-csenl.20` (H5: Host trait, natives, link) and the H4 money rules of
`ornadb-csenl.19` / `.17`. Written by agent `HirHostDesign`; measured at `origin/main` `491e275b4` (PR #9918 merged). The slice beads are
filed under epic `ornadb-csenl`; their ids are in section 3.

Two questions block executor agreement (`exec-share.toml` at `491e275b4`: `agreed = 49` of `evaluable = 110`; it was 48 before PR #9918):

1. **Part A.** `orna-exec` has no `Host` trait, so a `sys.*` read has nowhere to get data.
2. **Part B.** `Money<C>` is not declared anywhere in HIR, so `Money<GBP>`, `12.34.GBP`, `E5001`, `E5002` and `E5003`
   cannot be typed or run.

[hir.md](hir.md) section 4.4 and section 5.2 sketch the `Host` and `TableStore` traits;
[kernel-and-library.md](kernel-and-library.md) sections 5.7 and 7.4 decide that `Money<C>` is a library type and
sketch it; [sys-as-native.md](sys-as-native.md) section 3.5 decides how hosts register `sys` members. This document
does not reopen those decisions. It turns them into slices small enough for one implementer each.

Numbers below come from running `cargo test -p orna-differential --test exec_share` with `EXEC_SHARE_IDS=1` and a
scratch probe over the named cases (first at `30ef8fbe8`, the per-case table of section 0.2 re-measured at `491e275b4`), and from reading the named files. Anything not measured is marked
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
| `SysAdmission` (lowering-time view of linked `sys` members) | merged in PR #9918: `orna-hir/src/lower/admission.rs`; `CatalogueAdmission` in `orna-check` reads `orna_sys::system_provider_linkage()` | PR #9918 |
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
(`invalid/relation-equality.orna`, and it is not a `sys` case). Of the 47 evaluable cases the executor does not run at `491e275b4` (48 at `30ef8fbe8`, before #9918)
(measured breakdown at `30ef8fbe8`: 14 bind rows/arguments, 22 lower with a diagnostic (21 after #9918), 3 `relation operator (H7)`, 2 `money or type
constructor`, 1 each `as_of`, `engine native not ported`, `field access ... (H2)`, `intrinsic form`, `member call (H2)`,
`value use of a definition`, `no entry function`), the `sys` ones are:

| Case | Old pipeline outcome | HIR today (lowering diagnostics) | What closes it |
|---|---|---|---|
| `valid/sys-checkpoint` (`sys.Checkpoint.as_of(HEAD)`) | `sys.version.incompatible` | `` `sys` is a module, not a value `` | HA1 (lowering of `sys` value paths) + admission |
| `valid/sys-run-history` (`sys.Run.as_of(HEAD) \| sort_by(..)`) | `sys.version.incompatible` | same | HA1 |
| `valid/sys-table-query` (`sys.catalog.objects \| filter(.. sys.ObjectKind.table ..)`) | `sys.version.incompatible` | same, twice | HA1 |
| `valid/historical-query` (`contacts.Contact.as_of(sys.snapshot("HEAD~3"))`) | `sys.version.incompatible` | **done by PR #9918**: it now runs and agrees (the measured +1; step parity is 48 of 49 since) | none |
| `valid/historical-program` | `sys.version.incompatible` | four diagnostics, one is `energy.daily` unresolved | HA1, plus an `energy.daily` fixture function (harness, not Host) |
| `valid/presentation-watch` (`std.ui.Table(sys.rt.streams)`) | `sys.version.incompatible` | `Page` not exported, `sys` not a value | HA1, plus `std.ui` (separate native family, not in this document) |
| `valid/sys-storage` (`sys.Storage \| filter(..)`) | `ORNA-EVAL-UNSUPPORTED` | `sys` not a value | HA1, HA2, then HA5 (linked relation with no data) |
| `valid/cwd-head` (`contacts.Contact.as_of(CWD) \| count`) | `ORNA-EVAL-NAME` (harness has no store) | runs, stops at `as_of` | HA6, as a `[[harness-allowance]]` |
| `invalid/legacy-sys-runtime`, `legacy-sys-storage-call`, `mutate-sys-commit` | checker codes (`ORNA100-E-SYS-RUNTIME`, `ORNA100-E-SYS-STORAGE-CALL`, `E7001`) | the same codes, lowered statically (section 0.4) | none; agrees with the old pipeline |

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

### 0.4 Static `sys` rejections (ORNA-SYS-005, ORNA-SYS-120, E7001)

Three static `sys` rejections are the spec's own codes, so lowering reports them directly instead of
`ORNA-S022-UNSUPPORTED`. Each applies only to the exact path named, so any other `sys` path stays unrepresented:

- `sys.runtime.*` reports `ORNA100-E-SYS-RUNTIME` (ORNA-SYS-005: the member is renamed `sys.rt`, not aliased).
- The path `sys.storage` used as a callee reports `ORNA100-E-SYS-STORAGE-CALL` (ORNA-SYS-120: a grouping namespace).
- A row mutator on `sys.Commit` reports `E7001` (the read-only `sys.Commit` rule; published by the checker).

The codes are mirrored as string constants in `crates/orna-hir/src/diag.rs` (the checker owns the same strings in
`crates/orna-check/src/diagnostics.rs`; `orna-check` depends on `orna-hir`, so the strings cannot be imported). The
executor treats only these four codes as a shared static outcome (`orna-differential/src/observe_exec.rs`).

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

This is `ProgramCatalog` in `orna-exec/src/host/catalog.rs` (slice HA7). It needs no storage and no snapshot, and the
`Reference`/`Object` ids are derived from the same `ObjectId` scheme `Value::Enum` already uses (`variant_ids`).
`sys.Materialization`, `sys.MaintenanceJob` and `sys.Storage` need publication metadata (`orna-runtime`) and are not
in the first provider; with no data they answer `ORNA-EVAL-UNSUPPORTED`, which is what the old evaluator answers for
`valid/sys-storage`. `sys.rt.runs` and `sys.rt.streams` come from `orna-runtime`'s `SysCurrentRuntimeProjection`
(`orna-runtime/src/observation.rs:79`) in slice HA8. That projection states which fields the runtime does not own
(`FunctionRef`, `ObjectRef`, `InvocationRef`). HA8 does not `null` those non-null fields: the contract is frozen, so HA8
registers all 25 `sys.rt` fields (`orna-runtime/src/host_rows.rs`) and each member that cannot be answered honestly is
`ORNA-EVAL-UNSUPPORTED`. HA8 is PARTIAL; the member-to-bead map is in the `host_rows.rs` module table and row HA8 of 3.1.

### A6. The test provider for the differential harness

`orna-exec` (feature `test-support`) gets `FixtureHost`: a `StoreHost` plus a map `qualified name -> Vec<Raw>` of rows,
a fixed `ActivationContext`, a fixed clock and a counter entropy. Tests in `orna-exec` and the harness build it with
the rows they need, so no test depends on `orna-runtime` or a repository. The harness (`orna-differential`
`observe_exec`) keeps running the corpus on `StoreHost`: the old observer supplies no sys data either (0.2), so
`agreed` cannot depend on rows. `FixtureHost` is also what the REPL differential (H8) uses for `sys.rt.info`.

`valid/cwd-head` is the one corpus case that needs an activation snapshot. The old observer answers `ORNA-EVAL-NAME`
because it has no store, so the executor, once it runs `as_of`, returns the spec result (`{ cwd: 0, head: 0 }`) and
the case becomes a `[[harness-allowance]]` against `ornadb-csenl.21`, not an agreement. `HEAD` and `CWD` are not
declared anywhere in HIR today (the environment ratchet lists them under `CONNECTOR_AND_HOST`). Slice HA6 declares them
as `native static HEAD`/`CWD` of type `sys.SnapshotRef` in the root environment. `[INFERENCE]` the spec text names
`HEAD` as a root name (ORNA-STATE-003 `relation.as_of(HEAD)`) but I did not find the chapter that declares it; if the
owner declares them as `sys.database.head`/`.cwd` aliases instead, only that one declaration changes.

--------------------------------------------------------------------------

## 2. Part B: Money

### B1. What is decided and what this part adds

Decided elsewhere and not reopened: `Money<C>` is a library type with no currency axis in the kernel (kernel-and-library.md
5.7, owner direction O4); `Rate<C, U>` is a separate library type; operators are protocol calls with the output as a
protocol parameter (D16, `core/ops.orna` has `Add`, `Sub`, `Mul`, `Div`, `Neg`, `Order`); `Money`'s wire form
`[Decimal, currency id]` (tag 60007) is kept by a registry in Rust (D17, P9-A); a call to a loop-free pure `core`
function costs one step (D18); `core` is pure (kernel-and-library.md 6.1). The spec rules are ORNA-MONEY-001 to -010
(`reference/Orna-1.1.0/source/05-types.md:276-298`).

This part adds: where each piece is declared, how the executor represents it, the exact diagnostic rules for
`E5001`, `E5002`, `E5003` (and `E5004`, `E5103`, which the same corpus cases need), and an ordered slice list.
`Rate<C, U>` (`valid/money-rate`, ORNA-MONEY-003) and currency conversion (ORNA-MONEY-005, a data-backed std
operation) are **out of scope**; `valid/money-rate` stays in the environment ratchet.

### B2. Declaration, in core Orna

`crates/orna-hir/interface/core/money.orna`, module `core.money`, a hand-written module with bodies like
`core/temperature.orna` (it must lower with zero diagnostics, `tests/embedded_clean.rs`):

```orna
use core.ops.{Add, Sub, Mul, Div, Neg, Order};
use core.{Ordering};

// ORNA-MONEY-004: the stable contract. A symbol is deliberately not a member.
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

// An exact amount in currency `C`. The currency is in the type, not in a field. Engine representation: the executor
// value `Money { amount, currency }` (tag 60007), bound by the P9-A registry (section B6).
pub type Money<C impl Currency> {
    pub amount: Decimal,
    impl Add<Self, Self>     { fn add(self, other: Self): Self = money_add(self, other); }
    impl Sub<Self, Self>     { fn sub(self, other: Self): Self = money_sub(self, other); }
    impl Mul<Decimal, Self>  { fn mul(self, factor: Decimal): Self = money_scale(self, factor); }
    impl Div<Decimal, Self>  { fn div(self, divisor: Decimal): Self = money_divide(self, divisor); }
    impl Neg<Self>           { fn neg(self): Self = money_negate(self); }
    impl Order               { fn compare(self, other: Self): Ordering = money_compare(self, other); }
}

native fn money_add<C impl Currency>(left: Money<C>, right: Money<C>): Money<C> effect none;
// money_sub, money_scale, money_divide, money_negate, money_compare, likewise
```

Why this shape, from the code as it is:

- **Instances forward to top-level natives** because a `native fn` is legal only at the top level of a module
  (`native_declaration_is_top_level_only` rejects it in `impl`, native-declarations.md section 3.14 test list). The
  instance bodies are ordinary Orna calls, so `a + b` lowers to `Callee::Source` of the instance and then
  `Callee::Native`, as hir.md 2.6 says, and D18 charges one step per loop-free pure `core` call.
- **`Self`, not `Money<C>`, in protocol arguments** because "a type's own name as a protocol argument inside its
  `impl` does not resolve yet" (the note at the top of `core/temperature.orna`). `Self` does.
- **A public `amount` field** like `Temperature.kelvin`. Reading it is the explicit decimal conversion that
  ORNA-MONEY-003 requires before money meets `Float`.
- **No `impl Display` in the first cut.** The kernel-and-library.md listing writes `"{C.code} {self.base}"`, which
  needs a static read through a type parameter (P3d, "`C.code` with `C impl Currency` does not resolve" `[probe]`).
  Formatting is the native `format` of section B7, which receives `C` as a type argument (`NativeFn` takes `&[Ty]`) and
  reads the currency's statics from the program itself. This removes P3d from the critical path.
- **Prelude export.** `Money` and `Currency` are root-environment names (spec 09-standard-library.md:7, 136). The
  `PreludeExports` of `lower/prelude.rs` gains both, so `Money<GBP>` resolves without `use`, which removes
  `Money` from the `CURRENCY_AND_UNIT` environment category.

### B3. Currency declarations

A currency is a nominal type that implements `Currency` in its own `impl` (spec example, ORNA-MONEY-004):
`pub type GBP { impl Currency { static code = "GBP"; static minor_digits = 2; } }`. The reference corpus uses `GBP`,
`EUR` and `USD` with no declaration: the old checker attaches them as fixture symbols. The harness supplies them as
Orna source, as it supplies `fixture.units` and `fixture.attached` (`fixture_environment.rs`): a `fixture.currencies`
module with the three declarations, imported by `import_fixture_units`. This removes `GBP`, `EUR`, `USD` from the
`CURRENCY_AND_UNIT` category; `kWh`, `mph`, `A`, `C` stay there until the unit fixtures land.

**Open point Q-B1 (decision needed, default stated).** `valid/currency.orna` declares `pub protocol Currency` itself and
then `impl Currency` on `GBP`. In HIR a module declaration shadows the root name, so that `Currency` is a different
`DefId` from `core.money.Currency`, and `Money<GBP>` would fail the bound `C impl core.money.Currency`. The old checker
holds the protocol as the Rust string `"Currency"` (kernel-and-library.md F02) and accepts any protocol of that name.
Default decided here: **a protocol is the currency protocol when it is `core.money.Currency` or when it is named
`Currency` and its members are exactly `static code: Str` and `static minor_digits: Int`** (the ORNA-MONEY-004
contract). This keeps `valid/currency` valid, and keeps `invalid/currency-static-symbol` (extra `static symbol`)
rejected, with one predicate in `orna-hir/src/engine_money.rs` that typeck and the executor both read (the pattern of
`engine_order.rs` and `engine_display.rs`). The alternative, record `valid/currency` as an expected difference and make
shadowing the protocol an error, costs nothing to switch to: the predicate is the only place that changes.

### B4. Construction: `12.34.GBP`, `Money<GBP>(x)`

- **Suffix.** `lower_field` already tries, in order, a unit suffix (`lower_suffix_unit`) and the built-in postfix table
  (`lower_numeric_postfix`). Add a third step (P2-A rule 2): when the name resolves to a *type* in scope (not a unit),
  lower `literal.Name` as `Callee::Ctor { def: Money }` applied to the literal, with result type `Money<Name>`. The
  literal is lowered exactly (an integer or decimal `Lit`, never through `Float`, ORNA-MONEY-010). A `Float` literal
  (`12.34f.GBP`) is `E5004`. Whether `Name impl Currency` is checked at lowering or typeck: **typeck**, because the
  `impl` may follow the use; a failure there is `ORNA-S012-UNRESOLVED` "unit suffix cannot be resolved", as today.
- **Call form.** `Money<GBP>(x)` is the same `Callee::Ctor` (call.rs: "An enum variant, nominal or `Money<C>`
  constructor"). `x: Decimal` or `Int` is accepted (an `Int` is exact); `Float` is `E5004` (`invalid/money-float`).
- **Executor.** `Callee::Ctor` carries no type arguments, so the executor reads the call expression's type
  (`body.exprs[id].ty` = `Adt(Money, [Adt(GBP)])`), resolves a type-parameter argument through the frame's type
  environment, and builds `Value::Money { amount, currency }`. `currency` is the 16-byte object id of the currency
  type, derived from its qualified name by the function that derives nominal type ids for source enums
  (`orna_value::enum_type_id(qualified_name(..))`, as `variants.rs` does). `[INFERENCE]` The old evaluator receives the
  currency id from the application's trusted `NominalDefinition` (`orna-evaluator/src/lib.rs:530-545`), not from a
  derivation. The first step of slice HB6 is to read how the application assigns it and to record the equality (or the
  difference) in a test against `valid/money-serialization`'s golden bytes before any value is built.
- **Operator calls (HB6).** A `+`, `-`, `<`, or `in` on a nominal operand lowers to a `Callee::Source` whose `targs` are
  the instance's protocol arguments, then `Self`, then the receiver's own type arguments (`typeck/operator.rs`
  `Implementation::targs`). The executor binds the instance's type parameters from these, so a generic receiver such as
  `Money<GBP>` runs without a caller environment. Unary `-` and every binary and membership operator use this path.

### B5. The `E5001`, `E5002`, `E5003` rules

The three codes are published (kernel-and-library.md 4.7: "A new 'no instance' code must keep them for the same
inputs"), and today they are produced by the old checker only (`orna-check/src/diagnostics.rs:65-70`). The four corpus
cases that need them are `evaluate = true`, so the old outcome is the code and the executor agrees only when HIR rejects
the same input with the same code. Measured inputs:

| Case | Source | Old code | Class |
|---|---|---|---|
| `invalid/incompatible-dimensions` | `90.days + 4.kWh` | `E5001` | quantity dimensions differ |
| `invalid/affine-addition` | `20.C + 5.C` | `E5002` | two affine points added |
| `invalid/currency-addition` | `10.GBP + 5.EUR` | `E5003` | two different currencies |
| `invalid/affine-sum` (`evaluate = false`) | `values \| sum` over `[Float<C>]` | `E5003` | absolute quantity summed |

Only the third is Money. They share one mechanism, because all are "the operator has no instance, and the kernel knows a
better class for the failure". The mechanism (slice HB4) is one pure function in `orna-hir/src/engine_codes.rs`, beside
`engine_money.rs`:

```rust
/// The published code for a failed binary operator, when the operand types name a known class of failure.
pub fn operator_rejection(program: &Program, op: BinOp, left: Ty, right: Ty) -> Option<&'static str>;
```

Typeck (`typeck/operator.rs`, the no-instance branch of `settle_operator`) calls it before it words the generic
message, so the code stays kernel-owned and the library supplies only text (P7-A: "Codes are stable and kernel-owned").
Rules, in the order they are tried:

| Operator and operand types | Code |
|---|---|
| `+`, `-`, `<`, `<=`, `>`, `>=` where the operands are `core.money.Money<C>` and `Money<D>`, `C != D` | `E5003` |
| `+`, `-`, `*`, `/`, comparison where one operand is `Money<_>` and the other is `Float` | `E5103` |
| `Money<C> * Money<C>` | the generic no-instance diagnostic (there is no such impl, kernel-and-library.md 5.7 table) |
| `+`, `-` where both operands are `core.temperature.Temperature` (affine points) | `E5002` |
| `+`, `-` where both operands are `Quantity` values of different dimensions (the existing `ORNA-UNIT-DIM` site in `typeck/quantity.rs`) | `E5001` |
| a fold (`sum`) over an affine point type | `E5003` |

`E5001` and `E5002` are **units**, not Money; they are in this document only because the brief groups the three codes
and they use the same mechanism. Slice HB5 adds them. The old code for `Money<C>(Float)` is `E5004`; for a `Float`
operand `E5103`. `ORNA-UNIT-DIM` stays the code of the unit-family declaration rule (units.md ORNA-UNIT-010); only the
operator-level mismatch is `E5001`. Decision Q11 of hir-cutover.md (accept named differences) is not needed for these
codes: they are equal by construction.

### B6. Executor representation and arithmetic

`Value::Money { amount: Decimal, currency: [u8; 16] }` already exists and already round-trips through `to_raw` and
`raw_value::money_from`. The registry (P9-A) is the fact that the `core.money.Money` definition **is** that value:
`orna-exec/src/engine_money.rs` names the path `core.money.Money`, and a build-time unit test asserts the declaration
exists with one type parameter and the field `amount`, so a rename of the `core` path fails a test (D17: renaming is an
ABI bump).

The natives of B2 (`money_add`, `money_sub`, `money_scale`, `money_divide`, `money_negate`, `money_compare`) are
registered under `("core.money", name)` in `natives/mod.rs`, in a new `natives/money.rs`. Semantics are copied from the
old evaluator, not redesigned: `money_pair_binary` and `money_binary` (`orna-evaluator/src/lib.rs:7067-7092`,
`:7616-7664`), checked by the differential harness. Each charges one step (D18). Two amounts with different `currency`
ids cannot reach a native (E5003 is static); if they do, the native answers `ORNA-EVAL-TYPE`. `==` and `!=` stay
canonical equality, which includes the currency id. The field read `m.amount` is `ExprKind::Field` on a
`Value::Money`, a one-case addition to the executor's field read.

### B7. `quantize`, `allocate`, `format`

The old intrinsics are `std.money.{quantize, allocate, format}` over a generic `T` (`orna-intrinsics/src/modules/money.rs`):
`quantize(amount, minor_digits, rounding)`, `allocate(amount, weights, minor_digits, rounding = null)` and
`format(amount, currency_code, minor_digits, rounding, locale)`. They stay `std.money` forwarders in the pinned std
repository (a separate repository; this document changes nothing there). Under D12 the implementations are engine
natives declared in `core.money`:

```orna
pub native fn quantize<C impl Currency>(amount: Money<C>, rounding: Rounding): Money<C> effect none;
pub native fn allocate<C impl Currency>(amount: Money<C>, weights: [Int], rounding: Rounding? = null): [Money<C>] effect none;
pub native fn format<C impl Currency>(amount: Money<C>, rounding: Rounding, locale: Locale): Str effect pinned_data;
```

- `minor_digits` and `currency_code` are no longer arguments: the native reads `C.code` and `C.minor_digits` from the
  currency type's `impl Currency` through `targs[0]`, which is what the std wrapper passes today. The std wrapper keeps
  its `Str`/`Int` signature and is unchanged until the std repository moves. The generic `T` form (`quantize` of a bare
  `Decimal`) is not covered by these declarations; slice HB7 states what it covers, `[INFERENCE]` because the corpus does
  not exercise a bare-`Decimal` quantize.
- Rounding rules are those of `std/money.orna`: `half_even`, `half_up`, `toward_zero`, `away_from_zero`, `floor`, `ceil`.
  `core.Rounding` and `core.decimal.divide` already take that enum (`core/decimal.orna`), so the arithmetic is the
  shared `decimal_round.rs`, not a copy.
- Allocation uses "descending fractional remainder with input-index ties" (the intrinsic's own note), the method
  ORNA-MONEY-006 specifies.
- `format` has effect `PinnedData` in the old catalogue: symbols, grouping and decimal marks come from a captured locale
  profile (ORNA-MONEY-009). The effect name in a `core` declaration must be a `sys.EffectKind` member or `none`
  (native-declarations.md 3.8); if `pinned_data` is not one, the slice records the mapping it uses. The locale profile
  reaches the native through the `Host` (the `ActivationContext` of slice HA3 gains a `locale` field in HB7), so a browser
  build and `orna run` differ only by the host value. `format` is therefore the one money native that depends on part A,
  and is last in the order.

### B8. Harness consequences

- `observe_exec` already turns a `sys.version.incompatible` lowering diagnostic into the run outcome (PR #9918
  `admission_rejection`). HB8 generalises it: a typeck `E5001`-`E5004`/`E5103` diagnostic on an owned module is the run
  outcome, because the old pipeline's outcome for an `evaluate` case that its checker rejects is the checker's code.
  `[INFERENCE]` `observe_new::lower_case` may not run `typeck`; HB8 starts by checking and adds the call if not.
- `valid/currency`: the old outcome is `ORNA-EVAL-TYPE` (measured), the spec result is a `Money<GBP>` value. The
  executor will run it and disagree. It needs an allowance entry with a reason and bead; it cannot raise `agreed`.
- Expected movement, each verified by the slice's measured ratchet and not assumed: HA1 may add `valid/sys-checkpoint`,
  `valid/sys-run-history`, `valid/sys-table-query` to `agreed` (`valid/historical-query` already agrees since #9918); HA5 may add `valid/sys-storage`;
  HB8 may add `invalid/currency-addition`, `invalid/affine-addition`, `invalid/incompatible-dimensions`. That is up to 7
  of the 47 cases the executor does not run today (measured baseline: `agreed = 49` of 63 run, `evaluable = 110`).

--------------------------------------------------------------------------

## 3. Slices and beads

Every slice is one PR by one implementer, with the files, tests and acceptance below (the same text is in the bead).
Beads are children of epic `ornadb-csenl`. A slice never changes `layers.toml`; none needs to. The two parts are
independent except HB7 (needs HA3). Within a part the order is the dependency order.

### 3.1 Part A, in order

| Slice | Bead | Depends on | Files | Tests | Acceptance |
|---|---|---|---|---|---|
| HA1 lower `sys` value paths | `ornadb-csenl.25` | none | `orna-hir` `lower/namespace.rs`, `lower/exprs.rs`, `lower/calls.rs` | unit tests with a test `SysAdmission` over `sys.Run`, `sys.rt.runs`, `sys.catalog.objects`, `sys.ObjectKind.table`, `sys.snapshot("x")` | `valid/sys-checkpoint`, `sys-run-history`, `sys-table-query` lower with only `sys.version.incompatible`; measured `agreed` recorded (baseline 49) |
| HA2 typeck sys relation values | `ornadb-csenl.26` | HA1 | `typeck/call.rs`, `typeck/expr.rs` | `sys.Storage \| filter(..)` is `Relation<Storage>`; `sys.rt.runs \| count` is `Int` | tests pass; typed-share ratchet recorded |
| HA3 `Host`, `HostManifest`, `StoreHost` | `ornadb-csenl.27` | none | `orna-exec` `host.rs`, `machine.rs`, `lib.rs` | existing tests unchanged; host unit tests; manifest parity test vs `CatalogueAdmission` | `check_architecture.py` passes; no behaviour change |
| HA4 clock and entropy natives | `ornadb-csenl.28` | HA3 | `natives/time.rs` (or `clock.rs`), `natives/mod.rs` | `now` equal within one activation; `uuid7` deterministic under `FixtureHost` | `core.now`, `core.uuid7` bound |
| HA5 relation handles, provider source, `sys_call` | `ornadb-csenl.29` | HA3, HA2 | `value.rs`, `relation/pipeline.rs`, `eval.rs`, `host.rs` (`FixtureHost`) | `tests/sys_host.rs`: rows through `filter\|map\|take\|first\|count`; `take(1)` reads one row; registered member with no data is `ORNA-EVAL-UNSUPPORTED` | `valid/sys-storage` agrees; measured `agreed` recorded |
| HA6 snapshots, `HEAD`/`CWD`, `as_of` | `ornadb-csenl.30` | HA5 | `orna-hir/interface`, `table.rs`, `host.rs`, `store/memory.rs` | `as_of(HEAD)` reads the snapshot, not current; `sys.rt` handle refuses `as_of`; unknown snapshot is `sys.snapshot.not_found` | `valid/cwd-head` runs, recorded as a harness allowance |
| HA7 `ProgramCatalog` provider | `ornadb-csenl.31` | HA5 | `orna-exec` `host/catalog.rs`, `raw_value.rs` | `sys_catalog.rs`: tables and columns of a two-table program; `sys.rt.info` from `HostInfo` | rows match the declared tables; unlisted members stay unlinked |
| HA8 runtime provider, PARTIAL (`sys.rt`, 25 fields) | `ornadb-csenl.32` | HA5 | `orna-runtime` `host_rows.rs`, `orna-exec` `host/runtime_rows.rs`, `orna-exec/tests/runtime_rows.rs` | every `sys.rt` member links through `RuntimeHostRows`; `sys.rt.runs` is `ORNA-EVAL-UNSUPPORTED` (not "`count` is 2"): `sys.Run.function` (FunctionRef) and `sys.Stream.producer` (ObjectRef) are non-null and the runtime retains only names (`ornadb-jdq5h`); presentation scalars have no producer (`ornadb-jjgvi`); diagnostics members (`ornadb-5il1.280`); source-level `sys.rt.<member>` paths do not yet execute (`ornadb-5il1.249`) | `orna-exec` `runtime_rows` tests pass; `check_architecture.py` passes |

### 3.2 Part B, in order

| Slice | Bead | Depends on | Files | Tests | Acceptance |
|---|---|---|---|---|---|
| HB1 `core.money` declaration | `ornadb-csenl.33` | none | `interface/core/money.orna`, `embedded.rs`, `lower/prelude.rs`, `fixture_environment.rs`, `environment.rs` | `embedded_clean`; `Money<GBP>` resolves without `use` | zero diagnostics; environment counts re-measured |
| HB2 currency suffix and constructor | `ornadb-csenl.34` | HB1 | `lower/exprs.rs`, `lower/units.rs` or `lower/currency.rs`, `lower/calls.rs` | `12.34.GBP`, `10.GBP`, `Money<GBP>(5)` lower to `Callee::Ctor` with exact literals; `12.34f.GBP` carries the `E5004` marker | tests pass |
| HB3 `Currency` identity predicate | `ornadb-csenl.35` | HB1 | `engine_money.rs`, typeck bound check | `valid/currency` clean; `invalid/currency-static-symbol` rejected; other-member protocol not accepted | Q-B1 default implemented; owner can flip in one place |
| HB4 Money operator typing, `E5003`/`E5004`/`E5103` | `ornadb-csenl.36` | HB2, HB3 | `engine_codes.rs`, `typeck/operator.rs`, `diag.rs` | `typeck/money_tests.rs`, one test per B5 row | differing-rows file updated with measured codes |
| HB5 `E5001` and `E5002` | `ornadb-csenl.37` | HB4 | `engine_codes.rs`, `typeck/quantity.rs` | `90.days + 4.kWh`, `20.C + 5.C`, `values \| sum` over affine points | matching rows removed or updated |
| HB6 executor Money values and arithmetic | `ornadb-csenl.38` | HB2, HB3 | `orna-exec` `engine_money.rs`, `natives/money.rs`, `natives/mod.rs`, `eval.rs`, `registry_test.rs` | `native_money.rs`: sums, `amount`, compare, currency-id test against `valid/money-serialization` bytes | tests pass; first step records whether the derived currency id matches the old one |
| HB7 `quantize`, `allocate`, `format` | `ornadb-csenl.39` | HB6, HA3 | `core/money.orna`, `natives/money_round.rs` | all six rounding rules; `allocate(100.GBP, [1,1,1])` sums to 100; `format` takes the locale from the host | `registry_test` passes |
| HB8 harness outcome for `E50xx`, allowances, ratchets | `ornadb-csenl.40` | HB5, HB6 | `observe_exec.rs`, `exec-share.toml` | exec_share, diagnostic_codes, typed_share ratchets | `invalid/currency-addition`, `affine-addition`, `incompatible-dimensions` agree; `valid/currency` allowance recorded |

### 3.3 Open decisions

- **Q-B1** (section B3): how a user-declared `Currency` protocol relates to `core.money.Currency`. Default implemented in
  HB3. Owner ruling wanted before HB3 merges, but the default does not block HB1, HB2 or HB6.
- **`HEAD` and `CWD`** (section A6): root native statics (default) or aliases of `sys.database.head`/`.cwd`. Decided in
  the HA6 PR.
- Not decided here and not needed: `Rate<C, U>`, currency conversion (ORNA-MONEY-005), `std.ui` (blocks
  `valid/presentation-watch`), the blocking relation operators of H7 (`sort_by` and friends), `parallel` hosts.
