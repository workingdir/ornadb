use std::path::Path;

use orna_sys_v1::{
    system_host_operation_registry, system_host_operation_registry_json,
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
    let first_schema = build_host::generate_host_registry_schema().unwrap();
    let second_schema = build_host::generate_host_registry_schema().unwrap();
    assert_eq!(
        first_schema, second_schema,
        "host registry schema is deterministic"
    );
    assert_eq!(first_schema, system_host_operation_registry_schema_json());
    let schema: serde_json::Value = serde_json::from_str(&first_schema).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$defs"]["operation"]["additionalProperties"], false);

    let registry = system_host_operation_registry();
    let operations = registry.operations().collect::<Vec<_>>();
    assert_eq!(operations.len(), 15);
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
        "std.io.fs.create_dir",
        "std.io.fs.remove_file",
        "std.io.fs.copy_file",
        "std.io.fs.move_file",
        "std.net.http.send",
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
