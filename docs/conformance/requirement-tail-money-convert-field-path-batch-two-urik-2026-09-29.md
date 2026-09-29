# Requirement tail evidence: money, convert, field, path batch two

This batch adds ten narrow executable witnesses without changing the frozen
requirement register or production behavior. It is evaluated against the
frozen `Orna-1.0.0` publication. Every Orna source input is a real `.orna`
fixture loaded with `include_str!` in
`crates/orna-conformance-v1/tests/traceability_money_convert_field_path_batch_two.rs`.

`ORNA-TEST-004` is the evidence rule (frozen `Orna-1.0.0.md:4940`):
**Specified** means the cited normative clause exists; **Exists** means this
test and its source fixture are checked in; **Passed** means the named narrow
boundary passed in the recorded cargo run. A passed boundary is not a full
requirement pass.

## Per-row boundary

| Requirement | Specified: frozen anchor | Exists | Passed boundary |
| --- | --- | --- | --- |
| `ORNA-MONEY-001` | `Orna-1.0.0.md:641` | Yes: exact-decimal `.orna` fixture via `include_str!` | Exact decimal constructor typechecks with `Money<GBP>` result. |
| `ORNA-MONEY-002` | `Orna-1.0.0.md:643` | Yes: cross-currency-addition `.orna` fixture via `include_str!` | Unconverted GBP + USD is rejected with the analyzer's cross-currency type diagnostic. |
| `ORNA-MONEY-003` | `Orna-1.0.0.md:645` | Yes: exact and binary-Float `.orna` fixtures via `include_str!` | Exact Decimal rate expression typechecks; implicit binary Float entry is rejected. |
| `ORNA-MONEY-004` | `Orna-1.0.0.md:647` | Yes: complete/incomplete Currency `.orna` fixtures via `include_str!` | Complete static property shape typechecks; incomplete shape produces a type diagnostic. |
| `ORNA-CONVERT-001` | `Orna-1.0.0.md:797` | Yes: explicit `Target.from` `.orna` fixture via `include_str!` | Nested `From<Str>` selection typechecks and yields the target nominal type. |
| `ORNA-CONVERT-006` | `Orna-1.0.0.md:807` | Yes: implicit-conversion `.orna` fixture via `include_str!` | An unrequested implicit `From` application produces a type diagnostic. |
| `ORNA-FIELD-006` | `Orna-1.0.0.md:1490` | Yes: computed-field insertion `.orna` fixture via `include_str!` | Supplying the computed selector during insert is rejected. |
| `ORNA-FIELD-008` | `Orna-1.0.0.md:1494` | Yes: computed-effect `.orna` fixture via `include_str!` | Effectful computed expression is rejected as not deterministic and row-local. |
| `ORNA-PATH-007` | `Orna-1.0.0.md:1713` | Yes: new composite-key `.orna` fixture via `include_str!` | The fixture typechecks; storage path construction preserves the supplied declared-order components and appends `.orna` to the final component. |
| `ORNA-PATH-011` | `Orna-1.0.0.md:1721` | Yes: new composite-key `.orna` fixture via `include_str!` | Discovery rejects the tested non-canonical aliases `~61lice`, `~FF`, and `~e`. |

The boundary coverage intentionally stops at semantic type analysis and
`LoosePath` construction/discovery. It does not establish full compiler,
runtime, repository materialisation, symlink, or cross-platform behavior.

## Captured focused test result

Command:

```text
cargo test --locked --offline -p orna-conformance-v1 --test traceability_money_convert_field_path_batch_two -- --nocapture
```

The captured run printed each selected row as
`specified=yes exists=yes passed=yes` with the boundary name shown in the test
output, followed by:

```text
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CARGO_EXIT_CODE=0
```

## Remainder pinned by ID

Not selected in this batch: `ORNA-MONEY-005`, `ORNA-MONEY-006`,
`ORNA-MONEY-007`, `ORNA-MONEY-008`, `ORNA-MONEY-009`, `ORNA-MONEY-010`;
`ORNA-CONVERT-002`, `ORNA-CONVERT-003`, `ORNA-CONVERT-004`,
`ORNA-CONVERT-005`, `ORNA-CONVERT-007`, `ORNA-CONVERT-008`,
`ORNA-CONVERT-009`, `ORNA-CONVERT-010`;
`ORNA-FIELD-001`, `ORNA-FIELD-002`, `ORNA-FIELD-003`, `ORNA-FIELD-004`,
`ORNA-FIELD-005`, `ORNA-FIELD-007`, `ORNA-FIELD-009`, `ORNA-FIELD-010`,
`ORNA-FIELD-011`; `ORNA-PATH-001`, `ORNA-PATH-002`, `ORNA-PATH-003`,
`ORNA-PATH-004`, `ORNA-PATH-005`, `ORNA-PATH-006`, `ORNA-PATH-008`,
`ORNA-PATH-009`, `ORNA-PATH-010`, `ORNA-PATH-012`.

This list is only the remainder of this batch's selected ID set; it does not
claim that a listed requirement has no other repository evidence.
