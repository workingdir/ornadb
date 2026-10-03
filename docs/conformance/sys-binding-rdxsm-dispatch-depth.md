# Sys binding continuation 17 dispatch depth

The generated dispatch table is checked against every macro-generated sys
signature. The test reconstructs the signature from the typed parse tree and
compares it with the generated source descriptor, covering 66 routed
operations, 139 parameter types, 66 result types, and 51 defaulted parameters.
It confirms named, applied, list, and optional type shapes. Dispatch must select
the same typed contract, run all declared preconditions, and return the
reconstructed source signature. The matrix totals 271 operation and typed
field cases.

Focused test command from the repository root:

```text
cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_provider_abi dispatch_signature_depth_matrix_matches_every_typed_parameter_and_result -- --nocapture
```

Captured output and exit code:

```text
Blocking waiting for file lock on build directory
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-sys-v1)
    Finished `test` profile [unoptimized] target(s) in 18.99s
     Running tests/system_provider_abi.rs (/var/tmp/pbox-build/cargo/debug/deps/system_provider_abi-e3a50cdfd0882385)

running 1 test
dispatch_signature_depth_matrix operations=66 parameter_types=139 result_types=66 defaulted_parameters=51 type_shapes={"applied", "list", "named", "optional"} total_cases=271
test dispatch_signature_depth_matrix_matches_every_typed_parameter_and_result ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 11 filtered out; finished in 0.03s

EXIT_CODE=0
```

Broader gates were skipped as requested. The `orna` CLI was not invoked.
