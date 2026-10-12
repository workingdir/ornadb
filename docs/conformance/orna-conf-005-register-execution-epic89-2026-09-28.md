# ORNA-CONF-005 Register-Execution Evidence

Release epic: `ornadb-1787968123319-16-24513f57` (GitHub #89), register-execution wave 2

Base: `origin/main` at `fc3469fe1cc2c24f0ebe68bb45bfd657e62556a0`

Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`

Reference SHA-256 values:

| File | SHA-256 |
| --- | --- |
| `source/01-scope.md` | `26b8cc52da70ed17a0efd3fe2ef1b0da4f55a64e689b68e952dab24a0786ef06` |
| `tests/invalid-metadata.json` | `fd82672832318ceb8167906068bb91d8b6f3028a35528acb084bff730c14dc2e` |
| `tests/requirement-evidence.json` | `153c4c0f9678c52338dfd847e7aa706d888c9441d3b0924d013a6b7015d7a5aa` |

## Requirement and register state

The verified frozen clause at `source/01-scope.md:58` is:

> **ORNA-CONF-005** A language-processor claim MUST reject the invalid forms in the diagnostic corpus with the specified primary diagnostic class where the form can be identified.

The frozen `tests/requirement-evidence.json` entry remains `implementation_result: not executed`, its linked obligation remains `planned`, and `full_implementation_coverage_claimed` remains `false`. This run is bounded evidence; it does not complete the generic implementation-conformance obligation. The reference register was not edited.

## Executed linked corpus test

The linked audit runner is `crates/orna-conformance-v1/tests/conf005_diagnostic_corpus.rs`. It loads the frozen manifest and invalid metadata through `Corpus::load_default()` and evaluates the real `.orna` corpus files. It contains no inline Orna source. The runner checks the 78 parse, resolve, and typecheck cases and separately observes stages unsupported by this language-processor adapter.

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -q -p orna-conformance-v1 --test conf005_diagnostic_corpus -- --nocapture
```

Full captured output, including compiler warnings, is at `/tmp/ornadb-release-conf005-execution/conf005-diagnostic-corpus.log`.

Observed runner output:

```text
ORNA-CONF-005 audit: 76/78 parse, resolve, and typecheck cases matched; 2 evaluate/row-validation cases were skipped by this language-processor adapter.
  skipped: examples/invalid/duplicate-key.orna: Skipped
  skipped: examples/invalid/unsafe-row-key-repeat.orna: Skipped
Primary diagnostic mismatches: 2
  examples/invalid/legacy-result.orna: expected typecheck / ORNA091-E-RESULT, observed Failed / Some("ORNA-S012-UNRESOLVED")
  examples/invalid/two-durable-sources.orna: expected typecheck / E9102, observed Failed / Some("ORNA-S021-TYPE")
test reports_identifiable_language_processor_diagnostic_corpus_results ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

The two diagnostic mismatch lines and both skipped cases are retained verbatim from the run. Cargo exits 0 because the test reports mismatches instead of failing on them; this is not a CONF-005 pass. The two mismatches and two unsupported stages remain material. No conformance class or complete implementation result is claimed.
