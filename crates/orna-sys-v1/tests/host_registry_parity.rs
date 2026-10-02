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
    assert_eq!(operations.len(), 2);
    assert!(operations.iter().all(|operation| {
        operation.role == "host.std.io.environment@1.0"
            && operation.provider == "orna.sys.host.environment.v1"
            && operation.effects.len() == 1
            && operation.effects[0] == "read"
    }));
    let role = registry
        .role("host.std.io.environment@1.0")
        .expect("typed environment provider role");
    assert!(role.required);
    assert_eq!(role.operations.len(), operations.len());
}
