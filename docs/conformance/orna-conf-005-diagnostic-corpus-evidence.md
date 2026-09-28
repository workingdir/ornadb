# ORNA-CONF-005 diagnostic corpus evidence

Epic progress increment 6 for [OrnaDB v1.0.0 product release (#89)](https://github.com/workingdir/ornadb/issues/89). This record covers only ORNA-CONF-005; it is separate from the other CONF-lane records.

## Frozen requirement and inputs

The frozen reference states at `source/01-scope.md:58`:

> **ORNA-CONF-005** A language-processor claim MUST reject the invalid forms in the diagnostic corpus with the specified primary diagnostic class where the form can be identified.

SHA-256 observations from `/home/pbox/dev/ornadb/reference/Orna-1.0.0`:

| Reference file | SHA-256 |
| --- | --- |
| `source/01-scope.md` | `26b8cc52da70ed17a0efd3fe2ef1b0da4f55a64e689b68e952dab24a0786ef06` |
| `tests/conformance-manifest.json` | `fc81d58432e0249e71803206d09137703716e4a6637d4b798c7c11d311efd8ae` |
| `tests/invalid-metadata.json` | `fd82672832318ceb8167906068bb91d8b6f3028a35528acb084bff730c14dc2e` |

`tests/invalid-metadata.json` identifies 80 negative source fixtures and their expected failing phase, primary diagnostic, and message fragment. The manifest divides them into 35 parse, 2 resolve, 41 typecheck, 1 evaluate, and 1 row-validation cases. Corpus sources are loaded from the frozen `.orna` files by `Corpus::load_default`; the new Rust test contains no inline Orna source.

## Execution

The focused audit test at `crates/orna-conformance-v1/tests/conf005_diagnostic_corpus.rs` runs the frozen corpus through `Harness` and `SemanticAdapter`. It counts a case as matched only when the expected stage fails and the harness's adapter-normalized primary diagnostic and expected message fragment match the frozen metadata. It separately records the two stages the language-processor adapter skips.

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test -q -p orna-conformance-v1 --test conf005_diagnostic_corpus -- --nocapture
```

Captured test output:

```text
running 1 test
ORNA-CONF-005 audit: 76/78 parse, resolve, and typecheck cases matched; 2 evaluate/row-validation cases were skipped by this language-processor adapter.
  skipped: examples/invalid/duplicate-key.orna: Skipped
  skipped: examples/invalid/unsafe-row-key-repeat.orna: Skipped
Primary diagnostic mismatches: 2
  examples/invalid/legacy-result.orna: expected typecheck / ORNA091-E-RESULT, observed Failed / Some("ORNA-S012-UNRESOLVED")
  examples/invalid/two-durable-sources.orna: expected typecheck / E9102, observed Failed / Some("ORNA-S021-TYPE")
test reports_identifiable_language_processor_diagnostic_corpus_results ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.64s
CAPTURED_CARGO_EXIT_CODE=0
```

The cargo test exit code is 0 because this is an evidence runner that records mismatches; its green test result is not a full ORNA-CONF-005 pass. The run observed 76 of 78 parse, resolve, and typecheck cases matching their specified primary diagnostic. `legacy-result.orna` and `two-durable-sources.orna` were rejected with different primary classes from the frozen expectations. The evaluate case `duplicate-key.orna` and row-validation case `unsafe-row-key-repeat.orna` remain skipped by this adapter. All four observations prevent claiming full clause conformance from this run.

The reference's existing `tests/requirement-evidence.json` marks ORNA-CONF-005 `not executed`; this execution record does not modify or rewrite that frozen reference evidence.
