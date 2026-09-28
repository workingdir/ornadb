# ORNA-LIT / ORNA-NUM execution note — epic #89

Progress evidence for Beads epic `ornadb-1787968123319-16-24513f57` (GitHub #89). This note records selected repository tests executed on 2026-09-28 from source base `b9a063fa361329b92916dcdb97db1d823cea6209`; it is not a full clause conformance claim. Before documenting, I compared the seven later `origin/main` commits with the tested syntax, value, and evaluator crates; none changed those paths.

## Verified normative clauses

The frozen reference `/home/pbox/dev/ornadb/reference/Orna-1.0.0` states:

| Clause | Location | Verified requirement |
|---|---|---|
| ORNA-LIT-001 | `source/04-lexical.md:57` | Integer literals parse with arbitrary precision and are range-checked against bounded integer types. |
| ORNA-LIT-002 | `source/04-lexical.md:59` | Fractional/exponent literals retain exact parse form and default to Decimal; optional expected Float conversion uses correctly rounded binary64, ties-to-even; `f` selects Float. |
| ORNA-LIT-003 | `source/04-lexical.md:61` | `.decimal` explicitly selects Decimal and exact money/quantity contexts do not infer binary Float. |
| ORNA-LIT-004 | `source/04-lexical.md:63` | String interpolation uses `{ expression }` inside a double-quoted string. |
| ORNA-LIT-005 | `source/04-lexical.md:65` | ISO-compatible date/instant literals are recognized before splitting into integers and subtraction. |
| ORNA-LIT-006 | `source/04-lexical.md:67` | Decimal, Float, Date, and Instant lexical classes are disjoint. |
| ORNA-LIT-007 | `source/04-lexical.md:69` | A currency-typed decimal expression constructs exact Money only for a type implementing Currency; this is not a general implicit conversion or special declaration. |
| ORNA-NUM-001 | `source/05-types.md:81` | Int represents exact integers. |
| ORNA-NUM-002 | `source/05-types.md:83` | Decimal represents finite exact base-10 values, within implementation resource limits and without binary-float conversion. |
| ORNA-NUM-003 | `source/05-types.md:85` | Float uses IEEE-754 binary64 and is not the canonical representation of money. |
| ORNA-NUM-004 | `source/05-types.md:87` | Decimal addition, subtraction, and multiplication are exact unless a resource limit produces a typed Error. |
| ORNA-NUM-005 | `source/05-types.md:89` | Finite Decimal results remain exact; non-finite-decimal division fails unless rounding/precision is explicitly selected. |

## Register linkage

The frozen `tests/requirement-evidence.json` entries for ORNA-LIT-001–007 and ORNA-NUM-001–005 still have `implementation_result: not executed`, generic `implementation-conformance obligation` entries with `status: planned`, no executable test IDs, and `full_implementation_coverage_claimed: false`. `tests/requirements.json` records requirements, not test links. The frozen reference and its register were not modified. Because the register does not identify executable linked tests, the runs below are selected nearby implementation evidence rather than completion of the registered obligations.

## Executed tests

| Command | Actual Cargo output | Exit | Captured output |
|---|---|---:|---|
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-syntax-v1 --test lexer_literals -- --nocapture` | `14 passed; 0 failed` | 0 | `/var/tmp/ornadb-lit-num-lexer-literals.log` |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-value-v1 values_match_supplied_vectors -- --nocapture` | `1 passed; 0 failed; 32 filtered out` | 0 | `/var/tmp/ornadb-lit-num-value-vectors.log` |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-value-v1 bignums_reject_aliases_and_admit_the_first_out_of_range_magnitude -- --nocapture` | `1 passed; 0 failed; 32 filtered out` | 0 | `/var/tmp/ornadb-lit-num-bignums.log` |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-value-v1 float_vectors -- --nocapture` | `1 passed; 0 failed; 32 filtered out` | 0 | `/var/tmp/ornadb-lit-num-float-vectors.log` |
| `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -p orna-value-v1 fixture_numeric_vectors_are_exact -- --nocapture` | `1 passed; 0 failed; 32 filtered out` | 0 | `/var/tmp/ornadb-lit-num-numeric-vectors.log` |

The lexer test reads the checked-in `crates/orna-syntax-v1/tests/fixtures/lexer_literals.orna`; vector tests load frozen `float-vectors.json`, `numeric-vectors.json`, and value vectors through `include_str!`. These prove only the exercised inputs and assertions. In particular, this run does not establish arbitrary-precision source-literal checking, expected-type Float rounding, currency-constrained Money construction, or every arithmetic failure/resource-limit case. No failure occurred in these selected executions.

## Bounded conclusion

The fixture-backed lexer suite and selected numeric/value vector tests passed. They are partial implementation observations, not test links supplied by the frozen register and not evidence that all ORNA-LIT/ORNA-NUM requirements are satisfied. The planned register entries remain `not executed`; no broader claim is made.
