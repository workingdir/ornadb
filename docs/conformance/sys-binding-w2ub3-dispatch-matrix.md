# Sys binding continuation 16b dispatch matrix

Beads issue `ornadb-w2ub3` maps to GitHub issue [#7173](https://github.com/workingdir/ornadb/issues/7173).
The matrix derives positive selectors from the macro-generated API inventory and
checks each against the typed dispatch table. It covers 66 exact operation
routes, including the selected contract, signature, precondition count, and
role edge. It also derives one syntactically valid but unregistered selector
per callable family; all 41 are rejected before precondition or handler hooks
run. The test reports 107 cases total.

Focused test command from the repository root:

```text
cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_provider_abi dispatch_selector_conformance_matrix_covers_registry_routes_and_misses -- --nocapture
```

Captured output and exit code:

```text
   Compiling orna-value-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-value-v1)
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-sys-v1)
   Compiling orna-foundation-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-foundation-v1)
   Compiling orna-security-v1 v0.1.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-security-v1)
   Compiling orna-sys-macros v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-sys-macros)
   Compiling orna-syntax-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys2-20260930/crates/orna-syntax-v1)
    Finished `test` profile [unoptimized] target(s) in 7.67s
     Running tests/system_provider_abi.rs (/var/tmp/pbox-build/cargo/debug/deps/system_provider_abi-e3a50cdfd0882385)

running 1 test
dispatch_selector_conformance_matrix exact_routes=66 rejected_unregistered_selectors=41 total_cases=107
test dispatch_selector_conformance_matrix_covers_registry_routes_and_misses ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.02s

EXIT_CODE=0
```

The broader gates were skipped as requested. The `orna` CLI was not invoked.
