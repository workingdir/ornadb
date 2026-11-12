# Std natives survey (D12 move into core)

Status: survey of the pinned std (`stdlib/std`, commit fdf63e8) against the engine-embedded `core` interface
(`crates/orna-hir/interface/`) and the executor registry (`crates/orna-exec/src/natives/mod.rs`). Decision D12
(hir.md) moves the std natives into `core.orna` as body-less `native fn`. The std marker bodies
(`fail(error(... "is a native intrinsic"))`) are the call sites that the move replaces; `std` then re-exports core
with `pub use` (D11).

Method: the 82 unique marker names in std were mapped to `core.<module>.<name>`. The core side is every
`native fn` in `crates/orna-hir/interface` (81 declarations). The executor side is every `("core.<module>", "<name>")`
key in `natives::lookup` (76 keys), plus the root intrinsic `Some`, which is routed through
`Callee::Intrinsic(IntrinsicForm::OptionSome)` and does not use the registry.

## Per std module

| std module | std natives | core declared | executor bound | blocker |
| --- | --- | --- | --- | --- |
| `bits` | 6 | 6 of 6 (`core.bits`) | 6 of 6 | none |
| `hash` | 5 | 5 of 5 (`core.hash`) | 5 of 5 | none |
| `text` | 10 | 10 of 10 (`core.text`) | 10 of 10 | none |
| `stats` | 14 | 14 of 14 (`core.stats`) | 14 of 14 | none |
| `time` (offset_at, resolve_local, duration formats) | 6 | 6 of 6 (`core.time`) | 6 of 6 | none |
| `time/duration/{clock,compact,iso,words}` | 4 | `core.time.duration_*_format` | 4 of 4 | none (std spelling differs, same natives) |
| `encoding/base64` | 2 | 2 of 2 | 2 of 2 | none |
| `encoding/json` | 3 | 3 of 3 | 3 of 3 | none |
| `encoding/orna` | 2 | 2 of 2 | 2 of 2 | none |
| `encoding/ovb` | 2 | 2 of 2 | 2 of 2 | none |
| `collection` (group_by, rank, asof_join, bucket_by, last) | 5 | 5 of 5 (`core.collection`) | 4 of 5 | `bucket_by`: see below |
| `query` relation operators (count, filter, map, ...) | 17 | `core.collection.*` by name | 17 of 17 | none; std re-exports these names |
| `query` windows and time series (see below) | 6 | 0 of 6 | 0 of 6 | pure, unported; no core declaration |
| `money` (quantize, allocate, format) | 3 | 0 of 3 | 0 of 3 | `Money<C>` is not on HIR (H4); `format` also needs locale data |
| `random` (bytes, choose, integer, shuffle) | 4 | 0 of 4 | 0 of 4 | host entropy (`randomness` effect); no entropy host in orna-exec |
| `stream` (from_provider) | 1 | 0 of 1 | 0 of 1 | host provider effect; no provider host |
| `ui` (node) | 1 | 0 of 1 | 0 of 1 | presentation (P4) |
| root `Some` | (core) | 1 | intrinsic | implemented in this slice (`Callee::Intrinsic(OptionSome)`) |
| root `now` | (core) | 1 | 0 of 1 | host clock (`clock` effect) |
| root `uuid7` | (core) | 1 | 0 of 1 | host randomness (`randomness` effect) |
| root `error` | (core) | 1 | 0 of 1 | needs the structured `Error` type (`Value::Error` carries only a code; H2b/H5) |

## Unbound declarations

- `core.collection.bucket_by`: declared; `natives/collection/mod.rs` says it is not registered. Typeck returns
  `NotYetReason::Unsupported` for `RelOp::BucketBy` (`crates/orna-hir/src/typeck/call.rs`), so the period type `P` is
  not checked. The old evaluator's `BucketBySpec` (calendar and zone) is the reference.
- `core.now`, `core.uuid7`: effect-typed host natives (`clock`, `randomness`). Blocked on host hooks.
- `core.error`: needs a structured `Error` value; the executor's `Value::Error` holds only a `&'static str` code.
- `core.Some`: bound by the intrinsic route; registry-free.

## Families with no core declaration

- `money` (quantize, allocate, format): `Money<C>` is the blocker (H4). Until the money type lowers, the natives
  cannot take their generic `T`.
- `random` (bytes, choose, integer, shuffle): the spec has no seeded or deterministic random semantics (spec 09), so
  there is no reference behaviour to port. Blocked on an entropy host.
- `stream.from_provider`, `ui.node`: host provider and presentation effects. Not implementable in orna-exec today.
- `query` windows: `window_rate`, `window_rate_integrate`, `window_derivative`, `window_integrate`,
  `strictly_ordered_time_series`, `recursive_cte`. These are pure over `[(Instant, T)]` and have no core declaration.
  They are the only unblocked std natives that remain.

## Next steps

1. Interface declarations for the unblocked query windows, in their own commit in the core interface pattern
   (hand-written Orna, no JSON). Then executor bindings and tests.
2. `bucket_by` typeck and executor binding, once the period type `P` is checked.
3. Beads for the host-blocked families: clock and entropy (`now`, `uuid7`, `random.*`), provider streams
   (`stream.from_provider`), presentation (`ui.node`), money (H4), structured `Error` (H2b/H5).

## Window natives: why not in this slice

`window_rate`, `window_rate_integrate`, `window_derivative`, `window_integrate`, `strictly_ordered_time_series`, and
`recursive_cte` are host-free but not yet portable as a declaration-only change. The old evaluator implements the
window math in Rust (`crates/orna-evaluator/src/intrinsics/collection.rs`, from about line 826): rate and integrate
divide a generic `T` by `Instant` differences, so the core declaration must first fix the numeric constraint on `T`.
The std wrappers (`stdlib/std/query.orna`, lines 139-200) also validate window parameters before the native runs,
so the native contract must match that validation exactly. This is the next slice.
