# ORNA-SOURCE family execution evidence (epic #89)

## Verified contract

The frozen 1.0.0 reference defines:

- **ORNA-SOURCE-001** (`source/03-source-modules.md:7`): module-unit and row-unit parser entrypoints are distinct.
- **ORNA-SOURCE-002** (`source/03-source-modules.md:9`): module units contain declarations only at top level.
- **ORNA-SOURCE-003** (`source/03-source-modules.md:11`): row units contain exactly one record expression and no declarations.

The evidence rules in `source/32-conformance.md` distinguish specified tests, existing implementation tests, and passed executions (ORNA-TEST-004, line 23); require reporting the actual executable evidence and not treating a plan as execution (ORNA-TEST-010/011, lines 31/33); distinguish syntax probes from full implementation tests (Evidence levels, lines 65–69); and prohibit inferring a pass from a requirement-family label (ORNA-EVIDENCE-001, line 69). Neighboring `source/31-examples.md` describes the reference project fixtures; `source/33-diagnostics.md` covers diagnostic behavior and adds no ORNA-SOURCE requirement.

## Register state

The frozen `tests/requirement-evidence.json` maps ORNA-SOURCE-001/002/003 to an `implementation-conformance obligation` with `status: planned`; each remains `implementation_result: not executed` and `full_implementation_coverage_claimed: false` (reference lines 110–149). The entries do not name concrete test paths. This execution did not modify the frozen register, promote those statuses, or claim the family fully conforms.

## Executed evidence

All commands used `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0`. The complete captured stdout/stderr and exit codes are in `docs/conformance/orna-source-family-cargo-test-transcripts-epic89-2026-09-28.log`.

| Command | Actual result | Bounded interpretation |
| --- | --- | --- |
| `cargo test -p orna-syntax-v1` | Exit 0; 104 tests passed, 0 failed; doc tests 0 | Parser-package tests passed. This alone does not establish every distinct-entrypoint or module/row shape obligation. |
| `cargo test -p orna-conformance-v1 --test reference_corpus` | Exit 0; 44 passed, 0 failed | The reference-corpus integration tests passed. This is corpus-level evidence, not a claim that every ORNA-SOURCE clause has a dedicated assertion. |
| `cargo test -p orna-traceability-v1` | Exit 0; 29 passed, 0 failed; doc tests 0 | Traceability checks passed and preserve the distinction between planned obligations and executed evidence; they do not implement source-family behavior. |

No test failures occurred in these runs. Remaining source-family implementation obligations are still unexecuted in the frozen register. These results are recorded as bounded suite execution only, not as clause-by-clause ORNA-SOURCE conformance.
