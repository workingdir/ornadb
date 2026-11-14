# Engine-embedded interface modules

These files are the Orna source of the modules the engine embeds (`crates/orna-hir/src/embedded.rs`). They are of
two kinds.

## Core modules

No generator writes these files; they are hand-written Orna source (the generators are retired). Body-less `native fn`
declarations bind to the engine registry.

- `core.orna`
- `core/bits.orna`, `core/collection.orna`, `core/decimal.orna`, `core/hash.orna`, `core/money.orna`, `core/stats.orna`, `core/text.orna`, `core/time.orna`
- `core/ops.orna` (operator protocols, declarations with no natives)
- `core/encoding/base64.orna`, `core/encoding/json.orna`, `core/encoding/orna.orna`, `core/encoding/ovb.orna`

## Hand-written modules with bodies

Ordinary Orna source with bodies, dogfooded in the engine-embedded `core`. Edit them directly. `sys.orna` is the single
definition of the `sys` module (decision D19).

- `sys.orna`: the `sys` module, edited directly (see `docs/design/sys-as-native.md`).
- `sys/engine.orna`: the `sys.engine` module, the 61 engine failure codes as a private `FailureCode` enum.
- `core/display.orna`: the `Display` protocol that a string interpolation `"{x}"` calls (kernel-and-library.md 4.5, P5).
- `core/range.orna`: the `Range<T>` type of kernel-and-library.md listing 5, with `contains` and the `above`/`below`
  helpers.
- `core/temperature.orna`: the affine temperature scales of kernel-and-library.md listing 10 as library types:
  `Temperature` (a point) and `Delta` (a difference). The kernel has no affine flag (units.md C6). Their bodies use field
  access and struct literals, which HIR does not type yet, so those bodies are not type-checked; the instance signatures
  are.
- `core/units.orna`: the built-in data-size family (units.md section 8.2), plus the affine temperature `C` (offset
  273.15 over `K`) and the energy unit `kWh` the differential corpus uses (units.md sections 2 and 8.5). Values follow the
  checker's observer catalogue. The time family beyond `days` is not here; see the comment at the top of the file.

Hand-written modules must lower with zero diagnostics (`crates/orna-hir/tests/embedded_clean.rs`).
