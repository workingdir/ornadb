# Presentation and pages conformance audit (ornadb-y36t)

Base audited: `origin/main` at `6134a76e31999d4577cdd54b71926ee9bcc558bf`. Authority: frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/13-presentation.md` and `source/14-pages.md`. The requirement identifiers and line numbers below were checked directly in those two files. This audit does not edit the frozen reference register and does not claim complete chapter conformance.

Evidence grades: **A** = new executable fixture-backed direct witness; **B** = existing executable test or implementation witness with narrower scope; **C** = source implementation exists but the reviewed tests do not directly prove the complete clause; **D** = no direct implementation/conformance witness found in this bounded review or the observed implementation is an acknowledged residual.

## Top five proof gaps addressed by new tests

| Gap | Normative anchor | Prior evidence and grade | New evidence |
|---|---|---|---|
| A page is an ordinary function result, rather than a separate declaration form. | ORNA-PAGE-001, `source/14-pages.md:11` | Existing `authoritative_ui_catalogue_checks_page_builder_contextually` in `crates/orna-semantic-v1/tests/semantic_graph.rs:1577` checks one page expression (B). | `page_is_an_ordinary_function_result` analyzes the checked-in fixture and checks the exported function's `std.UI` result type (A). |
| Nested widgets/layouts compose through normal function values. | ORNA-PAGE-002, `source/14-pages.md:13` | The existing fixture demonstrates only one `Page` callback with `List` (B). | `page_layout_values_compose_through_ordinary_functions` checks helper, row, panel, column, and page functions in `page-composition.orna` (A). |
| Display implementations cannot write. | ORNA-PRES-008, `source/13-presentation.md:99` | Existing review regressions cover write rejection and effect propagation (B). | `display_implementation_write_is_rejected` loads the checked-in protocol/effect source fragments with `include_str!` and requires the type/effect diagnostic (A). |
| Present implementations cannot write. | ORNA-PRES-008, `source/13-presentation.md:99` | Existing review regressions cover write rejection and effect propagation (B). | `present_implementation_write_is_rejected` independently requires the same rejection for the Present protocol (A). |
| A secret value cannot be surfaced through Display. | ORNA-PRES-002, `source/13-presentation.md:25`; ORNA-PRES-008, `:99` | Existing semantic graph test covers the negative display diagnostic (B). | `secret_value_cannot_be_exposed_through_display` reruns the real checked-in secret fixture against the authoritative core catalogue (A for this prohibition only). It does not claim to prove every Inspect renderer redacts values. |

New proof command: `cargo test --locked --offline -p orna-semantic-v1 --test presentation_pages_y36t`. The run reported **5 passed, 0 failed**; the final captured output and exit code are attached to the PR.

## Remaining evidence gaps and bounded findings

- **D — PRES-001 (`source/13-presentation.md:12`) and PRES-004 (`:39`):** this change adds no end-to-end witness that terminal Display changes leave equality, hashing, storage, Git object IDs, queries, and codecs unchanged.
- **D — PRES-005 (`:67`):** no test in the reviewed presentation/page suites demonstrates that a session display override is ephemeral or explicitly saved. No behavior is inferred from the reference example alone.
- **C — PRES-006 (`:95`) and PRES-002 (`:25`):** Inspect-related implementation/test surfaces exist (`orna-core` inspect APIs and client inspect paths), but this slice does not establish cycle-safe bounded rendering and redaction end to end.
- **D — PRES-007 (`:97`):** the semantic implementation explicitly notes the built-in Display/Present protocol member surface is not carried in its local scope (`crates/orna-semantic-v1/src/lib.rs:3730`, current audit base). The new effect tests prove rejection only; they do not prove the required Display `Str` and core Present-tree signatures.
- **D — PRES-009 (`:101`) and PRES-010 (`:117`):** no end-to-end presenter-failure fallback-with-diagnostic or unknown-rich-node renderer fallback witness was found in the reviewed path. The typed protocol tree alone is not proof that all renderers fall back.
- **B/D — PAGE/LIVE/WIRE:** prior PAGE/LIVE work records partial selected evidence in `docs/conformance/page-live-evidence-execution-epic89-2026-09-28.md`; WIRE work records bounded selected evidence in `docs/conformance/wire-remote-test-execution-epic89-2026-09-28.md`. Those records explicitly bound their claims. They are not complete proof of ORNA-LIVE-001..004 or ORNA-WIRE-001..012. The client/server interoperability, loss/reorder/reconnect, malformed-input fuzz, slow-client and vector obligations at `source/14-pages.md:106` remain distinct.
- **Not upgraded by this slice — CODEC-001..006:** anchors are `source/13-presentation.md:131-155`. This test addition does not provide a complete typed codec, canonical encoding, row-key composition, or exact-source witness.
- **Not upgraded by this slice — ORNA-EVAL-001..011:** anchors are `source/14-pages.md:70-92`. Existing remote-evaluation/request-recovery tests are separate evidence; this page/presentation test file makes no broader claim.

The five new tests close selected executable-evidence gaps; the D/C items above remain explicit follow-up gaps rather than implied passes.
