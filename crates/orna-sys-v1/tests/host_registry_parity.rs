use std::path::Path;

use orna_sys_v1::{system_host_operation_registry, system_host_operation_registry_json};

#[path = "../build_host.rs"]
mod build_host;

#[test]
fn embedded_host_registry_matches_deterministic_annotated_method_projection() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let first = build_host::generate_host_registry(&source_root).unwrap();
    let second = build_host::generate_host_registry(&source_root).unwrap();
    assert_eq!(first, second, "host registry generation is deterministic");
    assert_eq!(first, system_host_operation_registry_json());

    let registry = system_host_operation_registry();
    let operations = registry.operations().collect::<Vec<_>>();
    assert_eq!(operations.len(), 4);
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
