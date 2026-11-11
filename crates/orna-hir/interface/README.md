# Engine-embedded interface modules

These files are the Orna source of the modules the engine embeds (`crates/orna-hir/src/embedded.rs`). They are of
two kinds.

## Generated (do not edit by hand)

Written by `scripts/gen_core_interface.py` from `crates/orna-intrinsics/std_intrinsics.json`. Each carries the
generated-file header. Body-less `native fn` declarations only.

- `core.orna`
- `core/bits.orna`, `core/collection.orna`, `core/hash.orna`, `core/stats.orna`, `core/text.orna`, `core/time.orna`
- `core/ops.orna` (operator protocols, generated declarations with no natives)
- `core/encoding/base64.orna`, `core/encoding/json.orna`, `core/encoding/orna.orna`, `core/encoding/ovb.orna`

`sys.orna` is hand-maintained (decision D19): it is the single definition of the `sys` module and no generator writes
it. `scripts/gen_interface.py` is the retired JSON-to-Orna bootstrap and refuses to run without `--bootstrap`.

Regenerate with `scripts/gen_core_interface.py`; `--check` exits 1 when a generated file is stale.

## Hand-written (plain Orna, not generated)

Ordinary Orna source with bodies, dogfooded in the engine-embedded `core`. The generator never writes these: it
lists them in `HAND_WRITTEN` and fails if one of them would be emitted. Edit them directly.

- `sys.orna`: the `sys` module, edited directly (see `docs/design/sys-as-native.md`).
- `sys/engine.orna`: the `sys.engine` module, the 61 engine failure codes as a private `FailureCode` enum.
- `core/range.orna`: the `Range<T>` type of kernel-and-library.md listing 5, with `contains` and the `above`/`below`
  helpers.

Hand-written modules must lower with zero diagnostics (`crates/orna-hir/tests/embedded_clean.rs`).
