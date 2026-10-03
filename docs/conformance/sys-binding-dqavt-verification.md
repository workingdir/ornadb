# Sys binding continuation 16 verification

Commands were run from the repository root with `--locked --offline`. The
focused exporter, generated-artifact parity, and dispatch tests passed. Broader
gates were skipped as requested; the `orna` CLI was not invoked.

```text
$ cargo test --locked --offline --manifest-path crates/orna-sys-v1/Cargo.toml --features dev-sys-export --test sys_api_export --test system_registry_parity --test system_provider_abi
running 5 tests
test dev_all_export_reconstructs_the_complete_embedded_artifact_tree ... ok
test dev_export_rejects_unknown_modes_and_extra_output_paths ... ok
test dev_provider_registry_export_is_byte_stable_schema_valid_and_runtime_identical ... ok
test dev_exports_are_byte_stable_and_api_export_matches_the_embedded_schema ... ok
test dev_exports_cover_embedded_host_registry_schema_and_binding_bundle ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 10 tests
test dispatch_table_enforces_registered_preconditions_and_failure_vocabularies ... ok
test provider_linkage_reports_missing_version_effect_and_duplicate_gaps ... ok
test provider_registry_role_edges_are_a_bijective_effect_compatible_sweep ... ok
test every_baked_role_resolves_its_offer_and_rejects_version_or_effect_widening ... ok
test generated_provider_abi_carries_typed_operation_contracts_and_roles ... ok
test every_dispatch_operation_matches_its_published_failure_vocabulary ... ok
test provider_diagnostic_codes_stay_outside_the_public_failure_catalog ... ok
test provider_abi_rejects_nonconformant_role_edges_and_effects ... ok
test dispatch_checks_each_precondition_and_stops_at_the_failing_position ... ok
test dispatch_enforces_the_full_failure_set_for_every_operation ... ok
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 6 tests
test dispatch_metadata_schema_covers_nullable_roles_and_rejects_unknown_or_invalid_fields ... ok
test dispatch_identifier_schema_and_typed_parser_reject_the_same_malformed_ids ... ok
test dispatch_metadata_schema_rejects_invalid_operation_and_role_fields_matrix ... ok
test dispatch_metadata_regeneration_conforms_across_every_operation_and_role ... ok
test generated_artifact_drift_probe_rejects_tampered_outputs_and_stale_modules ... ok
test generated_artifact_determinism_matrix_matches_embedded_and_build_outputs ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
[captured exit code: 0]
```
