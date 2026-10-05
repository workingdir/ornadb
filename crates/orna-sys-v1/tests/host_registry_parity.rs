use std::{collections::BTreeMap, path::Path};

use orna_syntax_v1::{Declaration, parse_module};
use orna_sys_v1::{
    ClockProviderError, EnvironmentDispatchValue, EnvironmentProvider, EnvironmentProviderError,
    FilesystemProviderError, HttpProviderError, ProcessProviderError,
    system_host_binding_modules_json, system_host_binding_stubs, system_host_operation_registry,
    system_host_operation_registry_json, system_host_operation_registry_schema_json,
};

#[path = "../src/abi_version.rs"]
mod abi_version;
#[path = "../src/host_registry_model.rs"]
mod host_registry_model;

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
fn generated_host_binding_artifacts_are_registry_backed_and_parse_as_orna_modules() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let host_registry = build_host::generate_typed_host_registry(&source_root)
        .expect("native host operations regenerate as a typed registry from Rust annotations");
    let host_registry_json = build_host::serialize_host_registry(&host_registry)
        .expect("typed host registry serializes deterministically");
    assert_eq!(host_registry_json, system_host_operation_registry_json());
    let artifacts = build_host::generate_host_binding_artifacts(&host_registry)
        .expect("typed native host registry emits binding declarations");
    assert_eq!(
        artifacts,
        build_host::generate_host_binding_artifacts(&host_registry)
            .expect("independent declaration generation is deterministic")
    );
    assert_eq!(artifacts.bundle, system_host_binding_stubs());
    assert_eq!(artifacts.modules_json, system_host_binding_modules_json());

    let out_dir = Path::new(env!("OUT_DIR"));
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_bindings.orna"))
            .expect("generated host binding bundle exists"),
        artifacts.bundle
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_binding_modules.json"))
            .expect("generated host module manifest exists"),
        artifacts.modules_json
    );
    let embedded_modules: BTreeMap<String, String> =
        serde_json::from_str(system_host_binding_modules_json())
            .expect("generated host module manifest is valid JSON");
    assert_eq!(embedded_modules, artifacts.modules);

    let registry = system_host_operation_registry();
    let mut parsed_operations = Vec::new();
    for (relative_path, expected_source) in &artifacts.modules {
        let source =
            std::fs::read_to_string(out_dir.join("system_host_bindings").join(relative_path))
                .unwrap_or_else(|error| {
                    panic!("read generated host module {relative_path}: {error}")
                });
        assert_eq!(&source, expected_source);
        let parsed = parse_module(&source);
        assert!(
            parsed.is_ok(),
            "generated host module {relative_path} parses: {:?}",
            parsed.diagnostics
        );
        let module = relative_path
            .strip_suffix(".orna")
            .expect("generated host module uses the Orna extension")
            .replace('/', ".");
        assert_eq!(
            source
                .lines()
                .find_map(|line| line.strip_prefix("// host-module: ")),
            Some(module.as_str())
        );
        let markers = source
            .lines()
            .filter_map(|line| line.strip_prefix("// host-op: "))
            .collect::<Vec<_>>();
        assert_eq!(markers.len(), parsed.value.items.len());
        for (marker, item) in markers.into_iter().zip(&parsed.value.items) {
            let operation = registry.operation(marker).unwrap_or_else(|| {
                panic!("emitted host declaration has registered operation {marker}")
            });
            let Declaration::Function { signature, .. } = &item.declaration else {
                panic!("generated host declaration {marker} is not a function");
            };
            assert_eq!(
                signature.name,
                operation.name.rsplit('.').next().unwrap(),
                "parsed declaration name for {marker}"
            );
            assert_eq!(
                operation.name.rsplit_once('.').map(|(parent, _)| parent),
                Some(module.as_str()),
                "generated module path for {marker}"
            );
            parsed_operations.push(marker.to_owned());
        }
    }
    assert_eq!(parsed_operations.len(), registry.operations().count());
    assert!(
        registry
            .operations()
            .all(|operation| parsed_operations.iter().any(|name| name == &operation.name)),
        "every typed native operation has one parsed emitted declaration"
    );
}

#[test]
fn generated_environment_declarations_match_executable_native_dispatch() {
    let registry = system_host_operation_registry();
    let role_name = "host.std.io.environment@1.0";
    let declarations = registry
        .binding_declarations_for_role(role_name)
        .expect("environment declarations are generated from the typed registry");
    assert_eq!(declarations.len(), 2);
    assert_eq!(
        declarations
            .iter()
            .map(|declaration| declaration.operation.as_str())
            .collect::<Vec<_>>(),
        ["std.io.environment.get", "std.io.environment.require"]
    );
    for declaration in &declarations {
        let operation = registry
            .operation(&declaration.operation)
            .expect("generated declaration has a registered native operation");
        assert_eq!(declaration.module, "std.io.environment");
        assert!(
            declaration
                .source
                .contains(&format!("// host-op: {}", operation.name))
        );
        let tail = operation
            .signature
            .strip_prefix(&format!("fn {}", operation.name))
            .expect("signature is keyed by operation name");
        let local_name = operation.name.rsplit('.').next().unwrap();
        assert!(
            declaration
                .source
                .contains(&format!("pub fn {local_name}{tail}"))
        );
    }

    let provider = EnvironmentProvider::from_snapshot([(
        "APP_MODE".to_owned(),
        Some("native-registry".to_owned()),
    )])
    .expect("host-approved environment snapshot");
    let get = registry.operation("std.io.environment.get").unwrap();
    assert_eq!(
        provider.dispatch(get, "APP_MODE"),
        Ok(EnvironmentDispatchValue::Optional(Some(
            "native-registry".to_owned()
        )))
    );
    let require = registry.operation("std.io.environment.require").unwrap();
    assert_eq!(
        provider.dispatch(require, "APP_MODE"),
        Ok(EnvironmentDispatchValue::Required(
            "native-registry".to_owned()
        ))
    );
}

#[test]
fn generated_tuple_return_provider_bindings_match_schema_and_typed_rows() {
    const TUPLE_OPERATIONS: [(&str, &str, &[&str], usize); 5] = [
        (
            "std.io.process.run",
            "fn std.io.process.run(executable: Str, arguments: [Str], working_directory: Str, environment: [(Str, Str)], input: Blob?, timeout: Duration?, max_output_bytes: Int): (Int?, Blob, Blob)",
            &[
                "executable",
                "arguments",
                "working_directory",
                "environment",
                "input",
                "timeout",
                "max_output_bytes",
            ],
            3,
        ),
        (
            "std.io.fs.metadata",
            "fn std.io.fs.metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)",
            &["root", "path"],
            4,
        ),
        (
            "std.io.fs.symlink_metadata",
            "fn std.io.fs.symlink_metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)",
            &["root", "path"],
            4,
        ),
        (
            "std.net.http.send",
            "fn std.net.http.send(method: Str, url: Str, headers: [(Str, Str)], request_body: Blob?, timeout: Duration?, max_header_bytes: Int, max_body_bytes: Int): (Int, [(Str, Str)], Blob)",
            &[
                "method",
                "url",
                "headers",
                "request_body",
                "timeout",
                "max_header_bytes",
                "max_body_bytes",
            ],
            3,
        ),
        (
            "std.net.http.wait",
            "fn std.net.http.wait(handle: Uuid, timeout: Duration?): (Int, [(Str, Str)], Blob)",
            &["handle", "timeout"],
            3,
        ),
    ];

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let registry_json = build_host::generate_host_registry(&source_root)
        .expect("tuple-return provider registry regenerates from Rust annotations");
    assert_eq!(registry_json, system_host_operation_registry_json());
    let schema_json = build_host::generate_host_registry_schema()
        .expect("tuple-return provider schema regenerates");
    assert_eq!(schema_json, system_host_operation_registry_schema_json());
    build_host::validate_host_registry_json(&registry_json, &schema_json)
        .expect("generated tuple-return provider rows conform to the generated schema");

    let out_dir = Path::new(env!("OUT_DIR"));
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_operations.json"))
            .expect("compiled host registry artifact exists"),
        registry_json,
        "compile-time host artifact preserves regenerated tuple signatures"
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("system_host_operations.schema.json"))
            .expect("compiled host schema artifact exists"),
        schema_json,
        "compile-time host schema equals its regenerated schema"
    );

    let raw: serde_json::Value =
        serde_json::from_str(&registry_json).expect("generated host registry is valid JSON");
    let rows = raw["operations"]
        .as_array()
        .expect("generated host registry has operation rows");
    let typed = system_host_operation_registry();
    let mut tuple_components = 0;
    for (name, expected_signature, expected_parameters, expected_arity) in TUPLE_OPERATIONS {
        let descriptor = typed
            .operation(name)
            .expect("tuple-return operation is in the typed host registry");
        let row = rows
            .iter()
            .find(|row| row["name"] == name)
            .expect("generated registry row exists");
        assert_eq!(
            descriptor.signature, expected_signature,
            "typed signature for {name}"
        );
        assert_eq!(row["signature"].as_str(), Some(expected_signature));
        assert_eq!(
            descriptor.parameters, expected_parameters,
            "typed parameter names for {name}"
        );
        let row_parameters = row["parameters"]
            .as_array()
            .expect("generated row parameter names are an array")
            .iter()
            .map(|parameter| {
                parameter
                    .as_str()
                    .expect("generated parameter name is a string")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            row_parameters, expected_parameters,
            "generated row parameter names for {name}"
        );

        let expected_result = expected_signature
            .rsplit_once("): ")
            .expect("tuple-return signature has a result separator")
            .1;
        assert!(expected_result.starts_with('(') && expected_result.ends_with(')'));
        let result_inner = &expected_result[1..expected_result.len() - 1];
        let (mut parentheses, mut brackets, mut angles, mut commas) = (0_i32, 0_i32, 0_i32, 0);
        for character in result_inner.chars() {
            match character {
                '(' => parentheses += 1,
                ')' => parentheses -= 1,
                '[' => brackets += 1,
                ']' => brackets -= 1,
                '<' => angles += 1,
                '>' => angles -= 1,
                ',' if parentheses == 0 && brackets == 0 && angles == 0 => commas += 1,
                _ => {}
            }
        }
        assert_eq!((parentheses, brackets, angles), (0, 0, 0));
        let arity = commas + 1;
        assert_eq!(arity, expected_arity, "tuple component count for {name}");
        tuple_components += arity;
    }

    assert_eq!(TUPLE_OPERATIONS.len(), 5);
    assert_eq!(tuple_components, 17);
    println!(
        "generated_host_tuple_return_parity operations={} tuple_shapes=3 tuple_components={tuple_components} typed_rows=5 compile_time_registry=1 schema_validated=1 total_cases=30",
        TUPLE_OPERATIONS.len()
    );
}

#[test]
fn tuple_return_provider_bindings_preserve_generated_dispatch_routes() {
    const EXPECTED_ROUTES: [(&str, &str, &str, &str); 5] = [
        (
            "std.io.process.run",
            "host.std.io.process@1.0",
            "orna.sys.host.process.v1",
            "invoke",
        ),
        (
            "std.io.fs.metadata",
            "host.std.io.fs.read@1.0",
            "orna.sys.host.filesystem.v1",
            "read",
        ),
        (
            "std.io.fs.symlink_metadata",
            "host.std.io.fs.read@1.0",
            "orna.sys.host.filesystem.v1",
            "read",
        ),
        (
            "std.net.http.send",
            "host.std.net.http@1.0",
            "orna.sys.host.http.v1",
            "invoke",
        ),
        (
            "std.net.http.wait",
            "host.std.net.http@1.0",
            "orna.sys.host.http.v1",
            "invoke",
        ),
    ];

    let generated_schema =
        build_host::generate_host_registry_schema().expect("tuple-return route schema regenerates");
    assert_eq!(
        generated_schema,
        system_host_operation_registry_schema_json()
    );
    build_host::validate_host_registry_json(
        system_host_operation_registry_json(),
        &generated_schema,
    )
    .expect("embedded tuple-return route rows conform to the generated schema");
    let raw: serde_json::Value = serde_json::from_str(system_host_operation_registry_json())
        .expect("embedded host registry is valid JSON");
    let rows = raw["operations"]
        .as_array()
        .expect("embedded host registry contains operation rows");
    let registry = system_host_operation_registry();

    let mut process_routes = 0;
    let mut filesystem_routes = 0;
    let mut http_routes = 0;
    for (name, role_name, provider_name, effect) in EXPECTED_ROUTES {
        let descriptor = registry
            .operation(name)
            .expect("tuple-return operation is in the typed host registry");
        let row = rows
            .iter()
            .find(|row| row["name"] == name)
            .expect("tuple-return operation has a generated JSON row");
        assert_eq!(descriptor.role, role_name, "typed role for {name}");
        assert_eq!(
            descriptor.provider, provider_name,
            "typed provider for {name}"
        );
        assert_eq!(descriptor.effects, [effect], "typed effect for {name}");
        assert_eq!(row["role"].as_str(), Some(role_name));
        assert_eq!(row["provider"].as_str(), Some(provider_name));
        assert_eq!(row["effects"][0].as_str(), Some(effect));

        let role = registry
            .role(role_name)
            .expect("tuple-return operation role is generated");
        assert_eq!(role.provider, provider_name);
        assert!(role.required);
        assert!(
            role.operations.iter().any(|operation| operation == name),
            "generated role {role_name} dispatches {name}"
        );
        match provider_name {
            "orna.sys.host.process.v1" => process_routes += 1,
            "orna.sys.host.filesystem.v1" => filesystem_routes += 1,
            "orna.sys.host.http.v1" => http_routes += 1,
            _ => unreachable!("expected tuple-return provider route"),
        }
    }

    assert_eq!(process_routes, 1);
    assert_eq!(filesystem_routes, 2);
    assert_eq!(http_routes, 2);
    println!(
        "generated_host_tuple_dispatch_routes operations={} process={process_routes} filesystem={filesystem_routes} http={http_routes} typed_roles=3 schema_validated=1",
        EXPECTED_ROUTES.len()
    );
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
