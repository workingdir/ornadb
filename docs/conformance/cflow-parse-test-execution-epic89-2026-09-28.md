# ORNA-CFLOW / ORNA-PARSE linked test execution (epic 89)

Date: 2026-09-28. Tracking epic: `ornadb-1787968123319-16-24513f57` (GitHub #89). Test base: `5791ef1b5fbb1116f9a60480bf988d748ce69787`.

## Normative scope checked first

The frozen reference at `/home/pbox/dev/ornadb/reference/Orna-1.0.0` was checked before test execution:

- `source/06-expressions.md:209-237` defines ORNA-CFLOW-001 through ORNA-CFLOW-011 and ORNA-PARSE-002.
- `source/06-expressions.md:267` defines ORNA-PARSE-001.
- `tests/scenarios.json` entry `CFLOW-001` links ORNA-CFLOW-001/002/003 and explicitly labels its evidence level “implementation scenario, not executed by an Orna engine.”
- `tests/scenarios.json` entry `LET-REBIND-091` links ORNA-CFLOW-005/006/011 and has the same bounded implementation-scenario evidence level.

## Frozen register status

`tests/requirement-evidence.json` was inspected but not edited. Every ORNA-CFLOW-001..011 and ORNA-PARSE-001/002 row remains `implementation_result: not executed`, with linked status `planned` and `full_implementation_coverage_claimed: false`. These runs are recorded separately as bounded observations; they do not rewrite the frozen register.

## Executed tests and captured results

The commands used `--locked --offline`. Full Cargo output was captured on the execution host in `/var/tmp/orna-cflow-parse-syntax.log`, `/var/tmp/orna-cflow-parse-runtime.log`, and `/var/tmp/orna-cflow-parse-let-rebind.log`.

1. `cargo test --locked --offline -p orna-syntax-v1` — **exit 0**.
   Captured results: unit tests `4 passed; 0 failed`; admission `2 passed; 0 failed`; syntax conformance `22 passed; 0 failed`; lexer literals `14 passed; 0 failed`; nesting limits `11 passed; 0 failed`; unary/postfix precedence `5 passed; 0 failed`; v1 review regressions `46 passed; 0 failed`; doctests `0 passed; 0 failed`. The syntax conformance target exercises the authoritative valid and invalid parse fixture corpus; the test source uses checked-in `.orna` fixtures and the frozen reference examples rather than adding inline Orna source here.
2. `cargo test --locked --offline -p orna-conformance-v1 --test runtime_scenarios control_flow_executes_the_frozen_contract_in_independent_evaluators -- --exact --nocapture` — **exit 0**.
   Captured result: `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out`. This executes the bounded CFLOW-001 implementation scenario, not an Orna engine.
3. `cargo test --locked --offline -p orna-conformance-v1 --test runtime_scenarios let_rebinding_executes_exact_runtime_checks_and_migration_diagnostic -- --exact --nocapture` — **exit 0**.
   Captured result: `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out`. This executes bounded LET-REBIND-091 scenario evidence for ORNA-CFLOW-005/006/011, not an Orna engine.

## Evidence boundary

These observations provide syntax/parser evidence for the executed syntax package and bounded adapter evidence for the two named scenarios. They do not establish full CFLOW/PARSE clause-family conformance or Orna-engine behavior. In particular, the two runtime scenarios do not independently prove every CFLOW clause, and no scenario-level evaluator result is presented as engine execution. The frozen register must not be changed based on these runs alone. No Orna source was added or embedded in new Rust test strings, so this evidence-only slice required no new `.orna` fixture.
