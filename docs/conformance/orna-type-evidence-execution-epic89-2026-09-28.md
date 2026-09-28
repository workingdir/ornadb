# ORNA-TYPE evidence execution

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Base: `origin/main` at `e18a9799adcf081145a2df751c7af992fba979c1`
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Verified clauses

The eight clauses below are in `source/05-types.md:45-59` and the register lists their IDs at `tests/requirement-evidence.json:785-890`.

- **ORNA-TYPE-001** (`source/05-types.md:45`): `T?` denotes `Option<T>`, not unchecked nullability.
- **ORNA-TYPE-002** (`:47`): failable expressions retain a successful value type and abrupt `Error` channel, not source `Result<T,E>` plumbing.
- **ORNA-TYPE-003** (`:49`): failure propagation cannot silently discard stream items; retry/preserve/skip/dead-letter remain explicit.
- **ORNA-TYPE-004** (`:51`): raw bytes and byte-count quantities must not share a type name.
- **ORNA-TYPE-005** (`:53`): closed variants use nominal enums; pipeline `|` is not a type-union operator.
- **ORNA-TYPE-006** (`:55`): `Result`, `Ok`, and `Err` are not reserved core constructors or variants.
- **ORNA-TYPE-007** (`:57`): user `type` declarations are transparent aliases, nominal types, or refined nominal types.
- **ORNA-TYPE-008** (`:59`): unannotated values still receive static inference or an inference diagnostic.

All eight frozen evidence rows still say `implementation_result: "not executed"` and carry only a generic planned obligation; they name no Cargo command. The frozen register was not edited. `tests/scenarios.json:1335-1350` maps `LEX-QUESTION-001` to ORNA-TYPE-001 among its requirements.

## Executed checks and limits

Exact Cargo commands, exit codes, and test summaries are in [`orna-type-evidence-execution-epic89-2026-09-28.log`](orna-type-evidence-execution-epic89-2026-09-28.log).

| Clause area | Actual result | Limit |
| --- | --- | --- |
| TYPE-001 | The `LEX-QUESTION-001` invalid-fixture diagnostic check passed. A semantic check over the real reference `values.orna` fixture also passed for `Str?`/optional matching. | These checks do not establish the complete `?`, `??`, `|?`, and legacy-call question-mark scenario end to end. |
| TYPE-002 | Fixture-backed recovery-effect test passed: handled failure clears `may_fail`; a fallback expression that can fail retains it. | Bounded semantic effect evidence, not proof of every abrupt `Error` path. |
| TYPE-003 | Runtime failed-delivery retention/retry regression passed for the same delivery identity. | Runtime adapter/storage evidence only; it does not prove source-level stream propagation or every skip/dead-letter rule. |
| TYPE-004 | No focused test was linked by the frozen register or selected here. | Not executed; no distinct-name conformance claim. |
| TYPE-005 / TYPE-007 | The real reference values fixture typechecked with its enum and refined `Score` declaration; the recovery fixture exercised pipeline `|`. | No test here rejects a type-union spelling or covers every alias/nominal form. |
| TYPE-006 | Fixture-backed user `Result` type and `Ok` function check passed. | Does not establish every possible user declaration named `Err`. |
| TYPE-008 | Fixture-backed omitted numeric parameter inference check passed with `square` inferred as `Int`. | One numeric function case only. |

No complete ORNA-TYPE family conformance is claimed. Three existing semantic tests now load source from real `.orna` fixtures with `include_str!`; `reference-values-type-family.orna` was copied from the frozen `examples/reference/values.orna`. The reference parser check reads the frozen `.orna` inputs through its existing reference-file helper. No clause status in the frozen register was promoted.
