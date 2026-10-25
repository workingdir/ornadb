default: check

# Run the default local fmt/build/lint/non-ignored test/rustdoc gate.
check: fixture-audit editor-artifacts-check fmt build lint test lsp-syntax-v1-parity sys-artifact-ci rustdoc-check

# Reject external reference paths, cross-crate includes, orphan source fixtures, and a checkout-local reference tree.
fixture-audit:
    test ! -e reference || (echo "fixture-audit: remove the top-level reference tree" >&2; exit 1)
    python3 -m unittest scripts.test_fixture_manifest
    python3 scripts/check_fixture_manifest.py --check
    cargo test --locked -p orna-syntax-v1 --test fixture_audit
    cargo test --locked -p orna-lsp --test ji3t0_conformance --test editor_attachment_completion
    cargo test --locked -p orna-sys-v1 --test sys_api_generation --test system_binding_stubs --test system_provider_abi
    cargo test --locked -p orna-runtime-v1 --test publication_metadata --test publication_repository_conformance
    cargo test --locked -p orna-runtime-v1 --test checkpoints_conformance_audit
    cargo test --locked -p orna-evaluator-v1 --test standard_snapshot_replay
    cargo test --locked -p orna-evaluator-v1 --test standard_library_hash_snapshot_27d5k --test standard_library_random_snapshot_27d5k
    cargo test --locked -p orna-evaluator-v1 --test standard_library_list_reverse_multiset_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_keys_stable_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_entries_order_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_list_concat_identity_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_conflict_chain_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_empty_side_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_boundary_pins_ln4y7
    cargo test --locked -p orna-evaluator-v1 --test standard_library_format_strings_si45g
    cargo test --locked -p orna-evaluator-v1 --test standard_library_map_conflicts_ln4y7
    cargo test --locked -p orna-runtime-v1 --test historical_snapshots

# Regenerate every editor package artifact from orna-syntax-v1.
editor-artifacts:
    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts

# Byte-check generated files and reject any unlisted hand-maintained editor file.
# This target is part of `just check` so editor artifacts cannot drift silently.
editor-artifacts-check:
    cargo test --locked -p orna-syntax-v1 --test editor_artifacts
    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check
    node --check editors/tree-sitter-orna/grammar.js


# Verify formatting without changing source files.
fmt:
    cargo fmt --all -- --check


# Build all workspace API documentation and reject rustdoc warnings.
rustdoc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps

# Compile every workspace target.
build:
    cargo check --locked --workspace --all-targets

# Reject all Clippy warnings across workspace targets.
lint:
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Run workspace tests excluding #[ignore] tests.
test:
    cargo test --locked --workspace --all-targets

# Keep LSP parsing, fixtures, and completion vocabulary aligned with ORNA-LEX-007.
lsp-syntax-v1-parity:
    cargo test --locked -p orna-lsp keyword_completion_vocabulary_matches_lex_007_exactly
    cargo test --locked -p orna-lsp in_crate_fixtures_parse_with_the_frozen_1_0_frontend

# Verify provider dispatch metadata export/schema conformance and generated-artifact drift.
sys-artifact-ci: sys-binding-conformance-ci sys-dispatch-coverage-ci
    cargo test --locked -p orna-sys-v1 --features dev-sys-export --test sys_api_export --test system_registry_parity --test system_provider_abi
    cargo test --locked -p orna-evaluator-v1 --features orna-sys-v1/dev-sys-export --test generated_sys_host_binding_coverage --test standard_library_process_environment --test standard_library_filesystem_network

# Sweep generated declaration parsing, registry typing/dispatch, and embedded schema parity.
sys-binding-conformance-ci:
    cargo test --locked -p orna-sys-v1 --features dev-sys-export --test system_binding_stubs --test host_registry_parity --test sys_schema_regeneration --test sys_api_generation

# Exercise every generated host operation through its registered evaluator dispatch arm.
sys-dispatch-coverage-ci:
    cargo test --locked -p orna-evaluator-v1 --features orna-sys-v1/dev-sys-export --lib every_generated_host_operation_reaches_its_native_dispatch_arm

# Compatibility name for the static generated-editor drift check.
editor-tooling-check: editor-artifacts-check
