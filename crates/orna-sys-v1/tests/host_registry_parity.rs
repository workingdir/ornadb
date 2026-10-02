use std::path::Path;

use orna_sys_v1::{
    ClockProviderError, EnvironmentProviderError, FilesystemProviderError, HttpProviderError,
    ProcessProviderError, system_host_operation_registry, system_host_operation_registry_json,
    system_host_operation_registry_schema_json,
};

#[path = "../build_host.rs"]
mod build_host;

#[test]
fn embedded_host_registry_matches_deterministic_annotated_method_projection() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let first = build_host::generate_host_registry(&source_root).unwrap();
    let second = build_host::generate_host_registry(&source_root).unwrap();
    assert_eq!(first, second, "host registry generation is deterministic");
    assert_eq!(first, system_host_operation_registry_json());
    let out_dir = Path::new(env!("OUT_DIR"));
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_operations.json"))
            .expect("read build-generated host operation registry"),
        first,
        "the compiled host registry bytes match the fresh annotated-method projection"
    );
    let first_schema = build_host::generate_host_registry_schema().unwrap();
    let second_schema = build_host::generate_host_registry_schema().unwrap();
    assert_eq!(
        first_schema, second_schema,
        "host registry schema is deterministic"
    );
    assert_eq!(first_schema, system_host_operation_registry_schema_json());
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_operations.schema.json"))
            .expect("read build-generated host operation schema"),
        first_schema,
        "the compiled host schema bytes match the fresh generated schema"
    );
    build_host::validate_host_registry_json(&first, &first_schema)
        .expect("generated host registry conforms to its generated JSON Schema");
    let schema: serde_json::Value = serde_json::from_str(&first_schema).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$defs"]["operation"]["additionalProperties"], false);

    let registry = system_host_operation_registry();
    let operations = registry.operations().collect::<Vec<_>>();
    assert_eq!(operations.len(), 20);
    assert_eq!(
        registry
            .operation("std.io.environment.get")
            .unwrap()
            .parameters,
        ["name"]
    );
    assert_eq!(
        registry.operation("std.io.process.run").unwrap().parameters,
        [
            "executable",
            "arguments",
            "working_directory",
            "environment",
            "input",
            "timeout",
            "max_output_bytes"
        ]
    );
    assert_eq!(
        registry
            .operation("std.concurrent.sleep")
            .unwrap()
            .parameters,
        ["duration"]
    );
    for name in [
        "std.io.fs.read_text",
        "std.io.fs.write_text",
        "std.io.fs.append_text",
        "std.io.fs.exists",
        "std.io.fs.is_directory",
        "std.io.fs.list",
        "std.io.fs.metadata",
        "std.io.fs.symlink_metadata",
        "std.io.fs.create_dir",
        "std.io.fs.remove_file",
        "std.io.fs.copy_file",
        "std.io.fs.move_file",
        "std.net.http.send",
        "std.net.http.start",
        "std.net.http.wait",
        "std.net.http.cancel",
    ] {
        assert!(
            registry.operation(name).is_some(),
            "{name} is in generated registry"
        );
    }
    for (role_name, provider_name, expected_effect) in [
        (
            "host.std.io.environment@1.0",
            "orna.sys.host.environment.v1",
            "read",
        ),
        (
            "host.std.io.process@1.0",
            "orna.sys.host.process.v1",
            "invoke",
        ),
        (
            "host.std.concurrent.clock@1.0",
            "orna.sys.host.clock.v1",
            "invoke",
        ),
        (
            "host.std.io.fs.read@1.0",
            "orna.sys.host.filesystem.v1",
            "read",
        ),
        (
            "host.std.io.fs.write@1.0",
            "orna.sys.host.filesystem.v1",
            "invoke",
        ),
        ("host.std.net.http@1.0", "orna.sys.host.http.v1", "invoke"),
    ] {
        let role = registry.role(role_name).expect("typed provider role");
        assert_eq!(role.provider, provider_name);
        assert!(role.required);
        assert_eq!(role.effects, [expected_effect]);
        assert_eq!(
            role.operations.len(),
            operations.iter().filter(|op| op.role == role_name).count()
        );
        assert!(
            operations
                .iter()
                .filter(|op| op.role == role_name)
                .all(|op| { op.provider == provider_name && op.effects == [expected_effect] })
        );
    }
}

#[test]
fn generated_registry_schema_rejects_unknown_fields_and_malformed_failure_codes() {
    let mut registry: serde_json::Value =
        serde_json::from_str(system_host_operation_registry_json()).unwrap();
    let schema = system_host_operation_registry_schema_json();

    let mut malformed_code = registry.clone();
    malformed_code["operations"][0]["failures"][0] = serde_json::json!("vendor.failure");
    assert!(
        build_host::validate_host_registry_json(&malformed_code.to_string(), schema)
            .unwrap_err()
            .contains("invalid sys failure code"),
        "the published failure-code taxonomy is enforced by the artifact schema"
    );

    registry["unexpected"] = serde_json::json!(true);
    assert!(
        build_host::validate_host_registry_json(&registry.to_string(), schema)
            .unwrap_err()
            .contains("unexpected field"),
        "generated artifacts reject fields outside the published shape"
    );
}

#[test]
fn native_provider_error_taxonomies_match_the_generated_operation_vocabularies() {
    let registry = system_host_operation_registry();
    let taxonomies = [
        (
            "orna.sys.host.environment.v1",
            [
                EnvironmentProviderError::InvalidName.code(),
                EnvironmentProviderError::Denied.code(),
                EnvironmentProviderError::Unset.code(),
                EnvironmentProviderError::Unavailable.code(),
            ]
            .to_vec(),
        ),
        (
            "orna.sys.host.process.v1",
            [
                ProcessProviderError::Denied.code(),
                ProcessProviderError::InvalidRequest.code(),
                ProcessProviderError::TimedOut.code(),
                ProcessProviderError::OutputLimit.code(),
                ProcessProviderError::Unavailable.code(),
            ]
            .to_vec(),
        ),
        (
            "orna.sys.host.clock.v1",
            [
                ClockProviderError::Denied.code(),
                ClockProviderError::Cancelled.code(),
            ]
            .to_vec(),
        ),
        (
            "orna.sys.host.filesystem.v1",
            [
                FilesystemProviderError::Denied.code(),
                FilesystemProviderError::InvalidRequest.code(),
                FilesystemProviderError::NotFound.code(),
                FilesystemProviderError::AlreadyExists.code(),
                FilesystemProviderError::NotFile.code(),
                FilesystemProviderError::NotDirectory.code(),
                FilesystemProviderError::InvalidUtf8.code(),
                FilesystemProviderError::Unavailable.code(),
                FilesystemProviderError::OutputLimit.code(),
            ]
            .to_vec(),
        ),
        (
            "orna.sys.host.http.v1",
            [
                HttpProviderError::Denied.code(),
                HttpProviderError::InvalidRequest.code(),
                HttpProviderError::HeaderLimit.code(),
                HttpProviderError::BodyLimit.code(),
                HttpProviderError::TimedOut.code(),
                HttpProviderError::Unavailable.code(),
                HttpProviderError::Cancelled.code(),
            ]
            .to_vec(),
        ),
    ];

    for (provider, codes) in taxonomies {
        let operations = registry
            .operations()
            .filter(|operation| operation.provider == provider)
            .collect::<Vec<_>>();
        assert!(
            !operations.is_empty(),
            "{provider} has registered operations"
        );
        let registered_codes = operations
            .iter()
            .flat_map(|operation| operation.failures.iter().map(String::as_str))
            .collect::<std::collections::BTreeSet<_>>();
        let native_codes = codes
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            registered_codes, native_codes,
            "native {provider} error variants and registry failure vocabulary stay aligned"
        );
        for operation in operations {
            assert!(
                operation
                    .failures
                    .iter()
                    .all(|failure| native_codes.contains(failure.as_str())),
                "{} only publishes failures supported by {provider}",
                operation.name
            );
        }
    }
}
