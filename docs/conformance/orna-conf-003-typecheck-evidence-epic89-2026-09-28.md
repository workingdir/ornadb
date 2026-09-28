# ORNA-CONF-003 Focused Typecheck Evidence

**Progress on release epic #89. Scope:** one implementation behavior test; no
conformance class is claimed.

The frozen requirement says an implementation must pass applicable
conformance fixtures and behavioral tests before claiming the corresponding
class, and a parsed example does not prove type correctness or runtime
behavior (`reference/Orna-1.0.0/source/01-scope.md:54`). Its evidence registry
still records the overall obligation as planned/not executed
(`reference/Orna-1.0.0/tests/requirement-evidence.json:35-45`).

This focused evidence runs the implementation test
`semantic_adapter_keeps_type_errors_in_the_typecheck_phase` in
`crates/orna-conformance-v1/tests/semantic_runtime_adapter.rs`. The test loads
`fixtures/semantic-validation/type-error-bad-table.orna` with `include_str!`,
confirms semantic resolution passes, then confirms the analyzer rejects the
program during typecheck with its type diagnostic. This observes behavior
beyond fixture discovery or parsing.

Command:

```sh
cargo test --locked --offline -p orna-conformance-v1 --test semantic_runtime_adapter semantic_adapter_keeps_type_errors_in_the_typecheck_phase -- --exact --nocapture
```

Captured result:

```text
running 1 test
test semantic_adapter_keeps_type_errors_in_the_typecheck_phase ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out
CARGO_TEST_EXIT=0
```

This is one bounded evidence item only. It does not establish that every
applicable fixture or behavioral test passes, does not demonstrate runtime
behavior, and does not satisfy the full registry obligation or claim a
conformance class.
