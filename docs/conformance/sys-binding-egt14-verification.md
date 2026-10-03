# Sys binding artifact export verification

Commands were run from the repository root with `--locked --offline`. The
focused exporter and regeneration checks passed; broader gates were skipped as
requested. The `orna` CLI was not invoked.

```text
$ cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --test system_registry_parity
running 6 tests
test dispatch_metadata_schema_covers_nullable_roles_and_rejects_unknown_or_invalid_fields ... ok
test dispatch_identifier_schema_and_typed_parser_reject_the_same_malformed_ids ... ok
test dispatch_metadata_schema_rejects_invalid_operation_and_role_fields_matrix ... ok
test generated_artifact_drift_probe_rejects_tampered_outputs_and_stale_modules ... ok
test dispatch_metadata_regeneration_conforms_across_every_operation_and_role ... ok
test generated_artifact_determinism_matrix_matches_embedded_and_build_outputs ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
[captured exit code: 0]

$ cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --features dev-sys-export --test sys_api_export
running 4 tests
test dev_export_rejects_unknown_modes_and_extra_output_paths ... ok
test dev_provider_registry_export_is_byte_stable_schema_valid_and_runtime_identical ... ok
test dev_exports_cover_embedded_host_registry_schema_and_binding_bundle ... ok
test dev_exports_are_byte_stable_and_api_export_matches_the_embedded_schema ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
[captured exit code: 0]
```
