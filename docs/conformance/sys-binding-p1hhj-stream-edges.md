# Sys binding continuation 46 provider stream edges

Beads issue `ornadb-p1hhj` maps to GitHub issue [#7907](https://github.com/workingdir/ornadb/issues/7907).
The in-crate fixture pins the generated `sys.admin.pause_stream` and
`sys.admin.resume_stream` declarations. The focused test checks each fixture
against the typed provider operation and generated binding bundle, confirms
both use `sys.StreamRef` and return `Bool`, and links `sys.StreamRef` to the
`sys.Stream` observation relation. It validates the generated API and provider
registry against their schemas. A companion schema test rejects a changed API
constant and a non-`sys` enum key.

Focused stream binding command from the repository root:

```text
cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_binding_stubs generated_stream_control_edges_match_bindings_and_schema_contracts -- --nocapture
```

Captured output and exit code:

```text
Blocking waiting for file lock on package cache
Blocking waiting for file lock on package cache
Blocking waiting for file lock on package cache
Blocking waiting for file lock on build directory
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-sys-v1)
    Finished `test` profile [unoptimized] target(s) in 2.70s
     Running tests/system_binding_stubs.rs (/var/tmp/pbox-build/cargo/debug/deps/system_binding_stubs-f4d8fba7b3783c26)

running 1 test
stream_control_binding_edges operations=2 stream_reference=linked api_schema=valid provider_schema=valid total_cases=4
test generated_stream_control_edges_match_bindings_and_schema_contracts ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 19 filtered out; finished in 0.05s

EXIT_CODE=0
```

Provider schema regression command:

```text
cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_registry_parity dispatch_metadata_schema_covers_nullable_roles_and_rejects_unknown_or_invalid_fields -- --nocapture
```

Captured output and exit code:

```text
    Finished `test` profile [unoptimized] target(s) in 0.20s
     Running tests/system_registry_parity.rs (/var/tmp/pbox-build/cargo/debug/deps/system_registry_parity-13df3b00bd0d333f)

running 1 test
test dispatch_metadata_schema_covers_nullable_roles_and_rejects_unknown_or_invalid_fields ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 17 filtered out; finished in 0.03s

EXIT_CODE=0
```

Published API schema command:

```text
cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_registry_parity published_system_api_schema_validates_constants_and_dynamic_maps -- --nocapture
```

Captured output and exit code:

```text
Blocking waiting for file lock on build directory
   Compiling orna-sys-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-sys-v1)
   Compiling orna-syntax-v1 v1.0.0 (/home/pbox/dev/ornadb/herdr-domains/imp_sys1-20260930/crates/orna-syntax-v1)
    Finished `test` profile [unoptimized] target(s) in 1m 14s
     Running tests/system_registry_parity.rs (/var/tmp/pbox-build/cargo/debug/deps/system_registry_parity-13df3b00bd0d333f)

running 1 test
system_api_schema_validation valid=1 const_rejections=1 property_name_rejections=1 total_cases=3
test published_system_api_schema_validates_constants_and_dynamic_maps ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 17 filtered out; finished in 0.08s

EXIT_CODE=0
```

Broader gates were skipped as requested. The `orna` CLI was not invoked.
