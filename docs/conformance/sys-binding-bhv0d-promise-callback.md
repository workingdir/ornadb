# Sys binding continuation 47 provider promise callback edges

Beads issue `ornadb-bhv0d` covers the generated provider binding edge between
`sys.start<T>` and `sys.await<T>`. The in-crate `.orna` fixture pins both
generated declarations and a completion callback that consumes
`InvocationHandle<T>` and returns `InvocationResult<T>`. The binding proof
checks the fixture and complete generated IDL bundle against typed provider
contracts, macro descriptors, and the regenerated schema.

The dispatch proof adds schema-valid callback preconditions to the two
operations, carries a typed handle from `sys.start<T>` into `sys.await`, and
checks successful terminal results and result-witness mismatch diagnostics on
both direct and selected-provider routes. Focused proof tests were run from the
repository root:

```text
cargo test -p orna-sys-v1 --test system_binding_stubs generated_provider_promise_callback_binding_matches_schema_and_idl -- --exact --nocapture
cargo test -p orna-sys-v1 --test system_provider_abi provider_promise_callback_preserves_handle_and_terminal_result_edges -- --exact --nocapture
```

Captured batch output and exit codes:

```text
    Blocking waiting for file lock on build directory
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-sys-v1)
   Compiling orna-foundation-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-foundation-v1)
   Compiling uuid v1.26.0
   Compiling orna-syntax-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-syntax-v1)
    Finished `test` profile [unoptimized] target(s) in 1m 28s
     Running tests/system_binding_stubs.rs (/var/tmp/pbox-build/cargo/debug/deps/system_binding_stubs-29e8a2cf08e7d235)

running 1 test
generated_provider_promise_callback_binding_parity operations=2 schema_validated=1 schema_rows=2 typed_contracts=2 macro_bindings=2 idl_operations=2 handle_result_pairs=1 completion_callback=1 total_cases=12
test generated_provider_promise_callback_binding_matches_schema_and_idl ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out; finished in 0.03s

BINDING_EXIT_CODE=0
    Blocking waiting for file lock on build directory
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-sys-v1)
    Finished `test` profile [unoptimized] target(s) in 3.76s
     Running tests/system_provider_abi.rs (/var/tmp/pbox-build/cargo/debug/deps/system_provider_abi-47eb62c65b7be493)

running 1 test
provider_promise_callback_parity start=sys.start<T> await=sys.await schema_validations=2 generated_bindings=2 handle_result_pairs=1 direct_edges=2 selected_provider_edges=2 callback_visits=6 terminal_result_mismatches=2 total_cases=14
test provider_promise_callback_preserves_handle_and_terminal_result_edges ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 39 filtered out; finished in 0.02s

PROVIDER_EXIT_CODE=0
EXIT_CODE=0
```

Broader gates were skipped as requested.
