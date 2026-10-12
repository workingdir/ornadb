# Evidence checks and probe grammar audit

## Audit boundary

The frozen `evidence/checks.json` reports five reference-side commands, each
with `returncode: 0` and `pass: true`. Its declared scope is authoring, static
and syntax checks plus selected models; it explicitly says that no complete
Orna implementation run occurred (`evidence/checks.json:2`). Those recorded
results are treated as frozen evidence, not as commands re-executed by this
audit.

The status below grades how much of each reference check is reproduced by the
retained Rust implementation and its tests. “Partial” means there is a related
implementation surface, but the frozen check's complete scope is not shown to
match or execute through that implementation. No row is promoted to full
conformance from the reference script's own passing model.

## Declared check inventory

| Frozen check | Frozen record | Rust implementation alignment | Evidence and boundary |
|---|---|---|---|
| `tools/build_protocol_registry.py` | Pass, return code 0; 13 messages and 3 closed HTTP structures (`checks.json:5-10`) | Partial | `orna-protocol-v1` defines the wire `Message` variants and codes, validates envelope rules, and has a message round-trip test (`crates/orna-protocol-v1/src/lib.rs:131-192,325-355,2151-2160`). The frozen check generates JSON registry/schema files; this audit did not establish byte-for-byte or full-schema parity between those files and Rust types. Relevant requirement: ORNA-PROTO-001 (`source/30-protocol.md:9`). |
| `tools/syntax_probe.py` | Pass, return code 0; bounded grammar probe agrees on 166 cases (`checks.json:13-18`) | Partial | `orna-syntax-v1` contains the production lexer/parser (`crates/orna-syntax-v1/src/lexer.rs:305-406,641-693`; `src/parser.rs:885-906`). The new probe test checks selected lexical forms and malformed dates/instants/scalar escapes through that parser. It does not replay all 166 cases or perform resolution/type checking. Relevant requirements: ORNA-TEST-005 (`source/32-conformance.md:25`) and ORNA-SYNTAX-002 (`source/04-lexical.md:169`). |
| `tools/contract_models.py` | Pass, return code 0; selected models report 15/15 (`checks.json:21-26`) | Partial | The record describes selected reference algorithms, static checks and real-Git experiments and explicitly excludes a complete Orna implementation run, an independent Parquet reader/writer, and a network client/server. The retained crates have corresponding semantic/runtime/storage implementations, but this record alone does not show those 15 model cases executing through those implementations. Evidence must preserve the distinction in ORNA-TEST-004 (`source/32-conformance.md:23`) and ORNA-EVIDENCE-001 (`source/32-conformance.md:69`). |
| `tools/protocol_model.py` | Pass, return code 0; selected models report 5/5 (`checks.json:29-35`) | Partial | Rust protocol and live-server code exist, including typed messages and decoding; the frozen model explicitly omits independent client/server runs, complete tagged-value validation, authentication, and malformed-WebSocket fuzzing. These limits leave implementation evidence gaps under ORNA-PROTO-001 (`source/30-protocol.md:9`), ORNA-PROTO-002 (`source/30-protocol.md:23`), ORNA-PROTO-003 (`source/30-protocol.md:92`), and ORNA-PROTO-004 (`source/30-protocol.md:113`). |
| `tools/check_sources.py` | Pass, return code 0; 66/66 source blocks, 5/5 reference modules, 14/14 focused cases (`checks.json:37-42`) | Partial | The frozen tool imports the bounded syntax probe and reports parse agreement. It is not evidence of name resolution, type checking, or execution. The Rust parser is independently exercised by the new lexical test, but this audit did not reproduce the complete 66-block scan in Rust. ORNA-TEST-001 requires parse, resolution, and type checking before a valid-fixture claim (`source/32-conformance.md:17`); ORNA-EVIDENCE-001 also forbids inferring a pass from a file or manifest (`source/32-conformance.md:69`). |

## Probe grammar cross-check

`evidence/probe-grammar.lark` models the `module_unit`, `repl_input`, and
`row_unit` entry points and adds bounded ASCII recognizers for identifiers,
integers, floats, decimals, dates, instants, and strings (`:1-4` and its
terminal definitions near the end of the file). Its paired syntax-probe record
explicitly limits strings to opaque tokens and excludes interpolation and the
Unicode lexical profile (`checks.json:17`). Those are probe boundaries, not
Orna language restrictions.

The new `probe_grammar_lexical` Rust integration test loads checked-in `.orna`
fixtures with `include_str!` and confirms parser acceptance for radix and
separated integers, decimal and float forms, valid date/instant forms,
contextual keyword positions, Unicode identifiers, interpolation, and string
escapes. It also confirms that invalid date, instant, and Unicode-scalar
values retain their lexer diagnostics rather than being accepted as
expressions. This checks parser behavior only; it does not claim name
resolution, typing, or evaluation (ORNA-TEST-001,
`source/32-conformance.md:17`). The lexical rules for Unicode identifiers,
contextual keywords, numeric classes, interpolation, calendar forms, and
malformed-token handling are ORNA-LEX-005 (`source/04-lexical.md:25`),
ORNA-LEX-007 (`source/04-lexical.md:29`), ORNA-LIT-001..006
(`source/04-lexical.md:57-67`), and ORNA-SYNTAX-002
(`source/04-lexical.md:169`).

Focused executed evidence:

```text
cargo test --locked --offline -p orna-syntax-v1 --test probe_grammar_lexical
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```
