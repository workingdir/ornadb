use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use orna_sys_v1::{
    SystemDispatchTable, SystemEffect, SystemProviderAbi, system_api_json, system_api_schema_json,
    system_binding_modules_json, system_binding_stubs, system_dispatch_table,
    system_function_descriptor, system_host_operation_registry_json,
    system_host_operation_registry_schema_json, system_provider_abi_json,
    system_provider_abi_schema_json,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
#[path = "../build_provider.rs"]
#[allow(dead_code)]
mod build_provider;
#[path = "../build_support.rs"]
mod build_support;

const SYS_API_V1_SHA256: &str = "b569785bfaa204b366b2cee444c01a9aa8dd74c710852fdad925dcfae60a256f";

fn regenerate() -> build_support::GeneratedSysArtifacts {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let collector = build_support::collect_rust_sources(&manifest.join("src"))
        .expect("annotated implementation methods form a valid registry");
    let registry = collector
        .type_graph
        .clone()
        .expect("annotated implementation registry owns the sys type graph");
    let schema = collector
        .schema
        .clone()
        .expect("annotated implementation registry owns the schema contract");

    build_support::generate_sys_artifacts(&collector.functions, registry, &schema)
        .expect("annotated registry regenerates all sys binding artifacts")
}

fn generated_modules(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn visit(root: &Path, base: &Path, files: &mut BTreeMap<String, String>) -> Result<(), String> {
        let entries = fs::read_dir(root).map_err(|error| {
            format!(
                "read generated binding directory {}: {error}",
                root.display()
            )
        })?;
        for entry in entries {
            let path = entry
                .map_err(|error| format!("read generated binding entry: {error}"))?
                .path();
            if path.is_dir() {
                visit(&path, base, files)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension == "orna")
            {
                let key = path
                    .strip_prefix(base)
                    .map_err(|error| format!("generated file escaped binding root: {error}"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = fs::read_to_string(&path).map_err(|error| {
                    format!("read generated module {}: {error}", path.display())
                })?;
                files.insert(key, source);
            }
        }
        Ok(())
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn verify_generated_output_tree(
    root: &Path,
    artifacts: &build_support::GeneratedSysArtifacts,
    host_registry_json: &str,
    host_schema_json: &str,
    provider_schema_json: &str,
) -> Result<(), String> {
    for (relative_path, expected) in [
        ("api_sys.json", artifacts.api_json.as_str()),
        ("system_api_schema.json", artifacts.schema_json.as_str()),
        (
            "system_provider_abi.json",
            artifacts.provider_abi_json.as_str(),
        ),
        ("system_provider_abi.schema.json", provider_schema_json),
        (
            "system_binding_modules.json",
            artifacts.binding_modules_json.as_str(),
        ),
        ("system_bindings.orna", artifacts.binding_bundle.as_str()),
        ("system_host_operations.json", host_registry_json),
        ("system_host_operations.schema.json", host_schema_json),
    ] {
        let path = root.join(relative_path);
        let actual = fs::read_to_string(&path).map_err(|error| {
            format!("missing or unreadable generated artifact {relative_path}: {error}")
        })?;
        if actual != expected {
            return Err(format!(
                "generated artifact {relative_path} drifted from registry output"
            ));
        }
    }
    let actual_modules = generated_modules(&root.join("system_bindings"))?;
    if actual_modules != artifacts.binding_modules {
        let paths = actual_modules
            .keys()
            .chain(artifacts.binding_modules.keys())
            .collect::<BTreeSet<_>>();
        let changed = paths
            .into_iter()
            .find(|path| actual_modules.get(*path) != artifacts.binding_modules.get(*path))
            .map(String::as_str)
            .unwrap_or("unknown");
        return Err(format!(
            "generated artifact system_bindings/{changed} drifted from registry output"
        ));
    }
    Ok(())
}

fn copy_output_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for name in [
        "api_sys.json",
        "system_api_schema.json",
        "system_provider_abi.json",
        "system_provider_abi.schema.json",
        "system_binding_modules.json",
        "system_bindings.orna",
        "system_host_operations.json",
        "system_host_operations.schema.json",
    ] {
        fs::copy(source.join(name), destination.join(name))?;
    }
    fn copy_modules(source: &Path, destination: &Path) -> std::io::Result<()> {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let source_path = entry?.path();
            let destination_path = destination.join(source_path.file_name().expect("entry name"));
            if source_path.is_dir() {
                copy_modules(&source_path, &destination_path)?;
            } else {
                fs::copy(source_path, destination_path)?;
            }
        }
        Ok(())
    }
    copy_modules(
        &source.join("system_bindings"),
        &destination.join("system_bindings"),
    )
}

#[test]
fn generated_artifact_determinism_matrix_matches_embedded_and_build_outputs() {
    let regenerated = regenerate();
    let second_build = regenerate();
    assert_eq!(
        regenerated, second_build,
        "independent registry collection and generation runs must emit identical artifacts"
    );
    let out_dir = Path::new(env!("OUT_DIR"));
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let host_registry_json = build_host::generate_host_registry(&source_root)
        .expect("annotated native host operations regenerate deterministically");
    assert_eq!(
        host_registry_json,
        build_host::generate_host_registry(&source_root)
            .expect("second native host operation registry projection"),
        "host operation registry bytes are stable across independent source walks"
    );
    let host_schema_json = build_host::generate_host_registry_schema()
        .expect("native host operation schema regenerates deterministically");
    assert_eq!(
        host_schema_json,
        build_host::generate_host_registry_schema()
            .expect("second native host operation schema projection"),
        "host schema bytes are stable across independent generations"
    );
    let provider_schema_json = build_provider::generate_provider_registry_schema()
        .expect("typed provider schema regenerates deterministically");
    assert_eq!(
        provider_schema_json,
        build_provider::generate_provider_registry_schema()
            .expect("second typed provider schema projection"),
        "dispatch schema generation is stable across independent runs"
    );

    let api_hash = format!("{:x}", Sha256::digest(regenerated.api_json.as_bytes()));
    assert_eq!(
        api_hash, SYS_API_V1_SHA256,
        "on-demand API retains the frozen 1.0 bytes"
    );
    let embedded_api_json = system_api_json();
    let artifact_matrix = [
        (
            "api/sys.json",
            regenerated.api_json.as_str(),
            embedded_api_json.as_str(),
            "api_sys.json",
        ),
        (
            "sys API schema",
            regenerated.schema_json.as_str(),
            system_api_schema_json(),
            "system_api_schema.json",
        ),
        (
            "typed provider dispatch registry",
            regenerated.provider_abi_json.as_str(),
            system_provider_abi_json(),
            "system_provider_abi.json",
        ),
        (
            "typed provider dispatch schema",
            provider_schema_json.as_str(),
            system_provider_abi_schema_json(),
            "system_provider_abi.schema.json",
        ),
        (
            "generated sys binding-module manifest",
            regenerated.binding_modules_json.as_str(),
            system_binding_modules_json(),
            "system_binding_modules.json",
        ),
        (
            "generated Orna stubs",
            regenerated.binding_bundle.as_str(),
            system_binding_stubs(),
            "system_bindings.orna",
        ),
        (
            "host operation registry",
            host_registry_json.as_str(),
            system_host_operation_registry_json(),
            "system_host_operations.json",
        ),
        (
            "host operation schema",
            host_schema_json.as_str(),
            system_host_operation_registry_schema_json(),
            "system_host_operations.schema.json",
        ),
    ];
    for (artifact, generated, embedded, build_output) in artifact_matrix {
        assert_eq!(
            generated, embedded,
            "{artifact} embedded bytes match generation"
        );
        assert_eq!(
            fs::read_to_string(out_dir.join(build_output))
                .unwrap_or_else(|error| panic!("read {artifact} build output: {error}")),
            generated,
            "{artifact} build output bytes match fresh generation"
        );
    }
    verify_generated_output_tree(
        out_dir,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
        &provider_schema_json,
    )
    .expect("every build output exactly matches fresh typed-registry projections");

    let regenerated_table = SystemProviderAbi::from_json(&regenerated.provider_abi_json)
        .expect("regenerated dispatch table is valid");
    assert_eq!(system_dispatch_table(), &regenerated_table);
    let api: Value = serde_json::from_str(&regenerated.api_json).expect("regenerated API JSON");
    let schema: Value =
        serde_json::from_str(&regenerated.schema_json).expect("generated system API schema");
    let provider_schema: Value =
        serde_json::from_str(&provider_schema_json).expect("generated typed provider JSON Schema");
    let provider_registry: Value = serde_json::from_str(&regenerated.provider_abi_json)
        .expect("generated typed provider registry JSON");
    assert_eq!(
        build_support::canonical_pretty_json(&provider_registry).unwrap() + "\n",
        regenerated.provider_abi_json,
        "embedded dispatch metadata uses deterministic canonical serialization"
    );
    assert_eq!(
        build_support::canonical_pretty_json(&provider_schema).unwrap() + "\n",
        provider_schema_json,
        "dispatch schema serialization is canonical and deterministic"
    );
    build_host::validate_json_against_schema(&regenerated.provider_abi_json, &provider_schema_json)
        .expect("generated dispatch metadata matches its JSON Schema");
    assert_eq!(
        provider_schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        build_support::canonical_pretty_json(&schema).unwrap() + "\n",
        regenerated.schema_json,
        "embedded schema serialization is deterministic"
    );
    build_support::validate_published_schema_shape(&api, &schema)
        .expect("generated API document matches its embedded schema");
    let function_ids = api["functions"]
        .as_array()
        .expect("generated API functions")
        .iter()
        .map(|function| {
            function["name"]
                .as_str()
                .expect("registered API function ID")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let dispatch_ids = regenerated_table
        .operations()
        .map(|operation| operation.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        function_ids, dispatch_ids,
        "API and dispatch registry are 1:1"
    );
}

#[test]
fn dispatch_metadata_schema_covers_nullable_roles_and_rejects_unknown_or_invalid_fields() {
    let dispatch_json = system_provider_abi_json();
    let schema_json = system_provider_abi_schema_json();
    let registry: Value = serde_json::from_str(dispatch_json).expect("embedded dispatch JSON");
    let schema: Value = serde_json::from_str(schema_json).expect("embedded dispatch schema");

    build_host::validate_json_against_schema(dispatch_json, schema_json)
        .expect("the complete generated registry matches its JSON Schema");
    assert_eq!(
        SystemProviderAbi::from_json(dispatch_json).expect("typed dispatch registry"),
        *system_dispatch_table(),
        "exported registry metadata reparses to the runtime dispatch table"
    );
    assert_eq!(
        schema["$defs"]["operation"]["properties"]["role"]["type"],
        serde_json::json!(["string", "null"]),
        "unroled operations are represented as explicit nullable roles"
    );

    let mut unknown_field = registry.clone();
    unknown_field["operations"][0]["unexpected"] = Value::Bool(true);
    assert!(
        build_host::validate_json_against_schema(&unknown_field.to_string(), schema_json)
            .unwrap_err()
            .contains("unexpected field"),
        "dispatch operation schema rejects metadata outside the generated contract"
    );

    let mut invalid_effect = registry.clone();
    invalid_effect["operations"][0]["effect"] = Value::String("mutate".to_owned());
    assert!(
        build_host::validate_json_against_schema(&invalid_effect.to_string(), schema_json)
            .unwrap_err()
            .contains("outside the schema enum"),
        "dispatch operation schema restricts effects to the ABI vocabulary"
    );

    let mut invalid_failure = registry;
    let operation = invalid_failure["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|operation| !operation["failures"].as_array().unwrap().is_empty())
        .expect("at least one dispatch operation declares failures");
    operation["failures"][0] = Value::String("vendor.failure".to_owned());
    assert!(
        build_host::validate_json_against_schema(&invalid_failure.to_string(), schema_json)
            .unwrap_err()
            .contains("invalid sys failure code"),
        "dispatch schema restricts operation failures to the sys vocabulary"
    );
}

#[test]
fn dispatch_identifier_schema_and_typed_parser_reject_the_same_malformed_ids() {
    let dispatch_json = system_provider_abi_json();
    let schema_json = system_provider_abi_schema_json();
    let registry: Value = serde_json::from_str(dispatch_json).expect("embedded dispatch JSON");
    let schema: Value = serde_json::from_str(schema_json).expect("embedded dispatch schema");

    assert_eq!(
        schema["$defs"]["operation"]["properties"]["role"]["pattern"],
        r"^[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)*@[0-9]+\.[0-9]+$"
    );
    assert_eq!(
        schema["$defs"]["role"]["properties"]["name"]["pattern"],
        r"^[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)*$"
    );
    build_host::validate_json_against_schema(dispatch_json, schema_json)
        .expect("valid provider identifiers conform to the generated schema");

    let mut invalid_annotation = registry.clone();
    let operation = invalid_annotation["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|operation| operation["role"].is_string())
        .expect("at least one operation declares a semantic role");
    operation["role"] = Value::String("bad role@1.0".to_owned());
    assert!(
        build_host::validate_json_against_schema(&invalid_annotation.to_string(), schema_json)
            .unwrap_err()
            .contains("identifier pattern"),
        "the published schema rejects malformed role annotations"
    );
    assert_eq!(
        SystemProviderAbi::from_json(&invalid_annotation.to_string()),
        Err(orna_sys_v1::ProviderAbiError::InvalidRoleId),
        "the typed parser rejects the same malformed role annotation"
    );

    let mut invalid_role_name = registry.clone();
    invalid_role_name["roles"][0]["name"] = Value::String("bad role".to_owned());
    assert!(
        build_host::validate_json_against_schema(&invalid_role_name.to_string(), schema_json)
            .unwrap_err()
            .contains("identifier pattern"),
        "the published schema rejects malformed role identifiers"
    );
    assert_eq!(
        SystemProviderAbi::from_json(&invalid_role_name.to_string()),
        Err(orna_sys_v1::ProviderAbiError::InvalidRoleId),
        "the typed parser rejects the same malformed role identifier"
    );

    let mut invalid_provider = registry;
    let role = invalid_provider["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|role| role["builtin_provider"].is_string())
        .expect("a required built-in role declares its provider ID");
    role["builtin_provider"] = Value::String("bad provider".to_owned());
    assert!(
        build_host::validate_json_against_schema(&invalid_provider.to_string(), schema_json)
            .unwrap_err()
            .contains("identifier pattern"),
        "the published schema rejects malformed provider identifiers"
    );
    assert_eq!(
        SystemProviderAbi::from_json(&invalid_provider.to_string()),
        Err(orna_sys_v1::ProviderAbiError::InvalidProviderId),
        "the typed parser rejects the same malformed provider identifier"
    );
}

#[test]
fn dispatch_metadata_regeneration_conforms_across_every_operation_and_role() {
    const SHARED_PROVIDER_FAILURES: [&str; 3] = [
        "sys.abi.precondition_failed",
        "sys.abi.unavailable",
        "sys.abi.provider_failed",
    ];

    fn effect_name(effect: SystemEffect) -> &'static str {
        match effect {
            SystemEffect::Read => "read",
            SystemEffect::Invoke => "invoke",
            SystemEffect::Admin => "admin",
        }
    }

    let generated = regenerate();
    let repeated = regenerate();
    assert_eq!(
        generated.provider_abi_json, repeated.provider_abi_json,
        "independent typed-registry regeneration runs preserve dispatch metadata bytes"
    );
    assert_eq!(
        generated.provider_abi_json,
        system_provider_abi_json(),
        "embedded dispatch metadata is the generated registry output"
    );
    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("dispatch metadata schema regenerates from its generator");
    assert_eq!(
        generated_schema,
        system_provider_abi_schema_json(),
        "embedded dispatch schema is the deterministic generated schema"
    );

    let registry: Value = serde_json::from_str(&generated.provider_abi_json)
        .expect("regenerated dispatch metadata is JSON");
    let schema: Value = serde_json::from_str(&generated_schema).expect("dispatch schema is JSON");
    build_host::validate_json_against_schema(&generated.provider_abi_json, &generated_schema)
        .expect("regenerated dispatch metadata conforms to the generated schema");
    let table = SystemProviderAbi::from_json(&generated.provider_abi_json)
        .expect("regenerated metadata parses into the typed dispatch table");
    assert_eq!(&table, system_dispatch_table());

    let api_json = system_api_json();
    let api: Value = serde_json::from_str(&api_json).expect("published API is JSON");
    let api_functions = api["functions"]
        .as_array()
        .expect("published API has function inventory")
        .iter()
        .map(|function| (function["name"].as_str().expect("function name"), function))
        .collect::<BTreeMap<_, _>>();
    let operation_rows = registry["operations"]
        .as_array()
        .expect("dispatch metadata has operations");
    assert_eq!(operation_rows.len(), table.operations().count());
    assert_eq!(operation_rows.len(), api_functions.len());

    for row in operation_rows {
        let name = row["name"].as_str().expect("dispatch operation name");
        let contract = table
            .operation(name)
            .unwrap_or_else(|| panic!("missing typed dispatch contract for {name}"));
        let function = api_functions
            .get(name)
            .unwrap_or_else(|| panic!("missing public API declaration for {name}"));

        assert_eq!(
            row["version"]["major"].as_u64(),
            Some(contract.version.major.into())
        );
        assert_eq!(
            row["version"]["minor"].as_u64(),
            Some(contract.version.minor.into())
        );
        assert_eq!(
            row["signature"].as_str(),
            Some(contract.signature.source.as_str())
        );
        assert_eq!(row["signature"], function["signature"]);

        let effects = contract.effects.iter().map(effect_name).collect::<Vec<_>>();
        assert_eq!(effects.len(), 1, "operation {name} has one declared effect");
        assert_eq!(row["effect"].as_str(), Some(effects[0]));
        assert_eq!(row["effect"], function["effect"]);

        let preconditions = contract
            .preconditions
            .iter()
            .map(|precondition| precondition.declaration.as_str())
            .collect::<Vec<_>>();
        let metadata_preconditions = row["preconditions"]
            .as_array()
            .expect("dispatch preconditions are an array")
            .iter()
            .map(|precondition| precondition.as_str().expect("precondition string"))
            .collect::<Vec<_>>();
        assert_eq!(
            metadata_preconditions, preconditions,
            "preconditions for {name}"
        );

        let metadata_failures = row["failures"]
            .as_array()
            .expect("dispatch failures are an array")
            .iter()
            .map(|failure| failure.as_str().expect("failure code string").to_owned())
            .collect::<BTreeSet<_>>();
        let typed_failures = contract
            .failures
            .iter()
            .filter(|failure| !SHARED_PROVIDER_FAILURES.contains(&failure.as_str()))
            .map(|failure| failure.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            metadata_failures, typed_failures,
            "failure vocabulary for {name}"
        );

        let role = match (&contract.role, contract.role_version) {
            (Some(role), Some(version)) => Some(format!(
                "{}@{}.{}",
                role.as_str(),
                version.major,
                version.minor
            )),
            (None, None) => None,
            _ => panic!("role name/version must be present together for {name}"),
        };
        assert_eq!(row["role"].as_str(), role.as_deref(), "role for {name}");
    }

    let role_rows = registry["roles"]
        .as_array()
        .expect("dispatch role inventory");
    assert_eq!(role_rows.len(), table.roles().count());
    for row in role_rows {
        let name = row["name"].as_str().expect("role name");
        let role = table
            .role(name)
            .unwrap_or_else(|| panic!("missing typed semantic role {name}"));
        assert_eq!(
            row["version"]["major"].as_u64(),
            Some(role.version.major.into())
        );
        assert_eq!(
            row["version"]["minor"].as_u64(),
            Some(role.version.minor.into())
        );

        let metadata_effects = row["effects"]
            .as_array()
            .expect("role effects are an array")
            .iter()
            .map(|effect| effect.as_str().expect("effect string").to_owned())
            .collect::<BTreeSet<_>>();
        let typed_effects = role
            .effects
            .iter()
            .map(|effect| effect_name(effect).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(metadata_effects, typed_effects, "effects for role {name}");

        let metadata_operations = row["operations"]
            .as_array()
            .expect("role operations are an array")
            .iter()
            .map(|operation| operation.as_str().expect("operation ID string").to_owned())
            .collect::<BTreeSet<_>>();
        let typed_operations = role
            .operations
            .iter()
            .map(|operation| operation.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            metadata_operations, typed_operations,
            "operations for role {name}"
        );
        assert_eq!(row["required"].as_bool(), Some(role.required));
        assert_eq!(row["replaceable"].as_bool(), Some(role.replaceable));
        assert_eq!(
            row["builtin_provider"].as_str(),
            role.builtin_provider
                .as_ref()
                .map(|provider| provider.as_str()),
            "built-in provider for role {name}"
        );
    }

    assert_eq!(
        schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "the conformance matrix exercises the published schema dialect"
    );
}

#[test]
fn dispatch_metadata_schema_rejects_invalid_operation_and_role_fields_matrix() {
    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    let schema_json = system_provider_abi_schema_json();
    build_host::validate_json_against_schema(system_provider_abi_json(), schema_json)
        .expect("generated dispatch metadata conforms before drift probes");

    let operation_field_mutations = [
        ("name", Value::String(String::new())),
        ("version", serde_json::json!({"major": "one", "minor": 0})),
        ("signature", Value::String(String::new())),
        ("effect", Value::String("mutate".to_owned())),
        ("preconditions", serde_json::json!([""])),
        ("failures", serde_json::json!(["vendor.failure"])),
        ("role", Value::String(String::new())),
    ];
    for (field, invalid) in operation_field_mutations {
        let mut mutated = registry.clone();
        mutated["operations"][0][field] = invalid;
        assert!(
            build_host::validate_json_against_schema(&mutated.to_string(), schema_json).is_err(),
            "dispatch schema rejects invalid operation field {field}"
        );
    }

    let role_field_mutations = [
        ("name", Value::String(String::new())),
        ("version", serde_json::json!({"major": 1, "minor": "zero"})),
        ("effects", serde_json::json!(["mutate"])),
        ("operations", serde_json::json!([""])),
        ("required", Value::String("yes".to_owned())),
        ("replaceable", Value::String("no".to_owned())),
        ("builtin_provider", Value::String(String::new())),
    ];
    for (field, invalid) in role_field_mutations {
        let mut mutated = registry.clone();
        mutated["roles"][0][field] = invalid;
        assert!(
            build_host::validate_json_against_schema(&mutated.to_string(), schema_json).is_err(),
            "dispatch schema rejects invalid role field {field}"
        );
    }
}

#[test]
fn generated_schema_accepts_relationship_shapes_that_typed_dispatch_rejects() {
    let generated = regenerate();
    assert_eq!(
        generated.provider_abi_json,
        system_provider_abi_json(),
        "the embedded dispatch artifact is generated from the typed registry"
    );
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(
        schema_json,
        system_provider_abi_schema_json(),
        "the embedded dispatch schema matches deterministic generated output"
    );
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("the generated dispatch document conforms to its schema");
    assert_eq!(
        SystemProviderAbi::from_json(system_provider_abi_json())
            .expect("the generated dispatch document parses as a typed registry"),
        *system_dispatch_table(),
        "the schema-valid embedded document is the runtime dispatch table"
    );

    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch document");
    let assert_typed_relationship_rejection = |mutated: Value, expected| {
        let json = mutated.to_string();
        build_host::validate_json_against_schema(&json, &schema_json)
            .expect("relationship mutation retains the generated schema shape");
        assert_eq!(
            SystemProviderAbi::from_json(&json),
            Err(expected),
            "typed dispatch validation rejects relationship drift"
        );
    };

    let role_with_multiple_operations = registry["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|role| role["operations"].as_array().unwrap().len() > 1)
        .expect("the registry has a role with multiple operations");
    let original_role_name = role_with_multiple_operations["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let unknown_role_operation_name = role_with_multiple_operations["operations"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let role_annotation = registry["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["name"] == unknown_role_operation_name)
        .expect("role operation exists in the dispatch list")["role"]
        .as_str()
        .expect("role-bound operation declares a role");
    let (_, version) = role_annotation
        .rsplit_once('@')
        .expect("role annotation has a version suffix");
    let mut unknown_role = registry.clone();
    unknown_role["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|role| role["name"] == original_role_name)
        .expect("original role exists")["operations"]
        .as_array_mut()
        .unwrap()
        .retain(|operation| operation.as_str() != Some(unknown_role_operation_name.as_str()));
    let operation = unknown_role["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|operation| operation["name"] == unknown_role_operation_name)
        .expect("role operation exists in the dispatch list");
    operation["role"] = Value::String(format!("sys.cont23_missing_role@{version}"));
    assert_typed_relationship_rejection(
        unknown_role,
        orna_sys_v1::ProviderAbiError::OperationRoleMismatch,
    );

    let mut missing_reverse_link = registry.clone();
    let role = &missing_reverse_link["roles"][0];
    let operation_name = role["operations"][0]
        .as_str()
        .expect("role has an operation")
        .to_owned();
    let operation = missing_reverse_link["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|operation| operation["name"] == operation_name)
        .expect("role operation exists in the dispatch list");
    operation["role"] = Value::Null;
    assert_typed_relationship_rejection(
        missing_reverse_link,
        orna_sys_v1::ProviderAbiError::RoleOperationMissing,
    );

    let mut mismatched_role_version = registry;
    let role = &mut mismatched_role_version["roles"][0];
    let minor = role["version"]["minor"]
        .as_u64()
        .expect("role minor version is an integer");
    role["version"]["minor"] = Value::from(minor + 1);
    assert_typed_relationship_rejection(
        mismatched_role_version,
        orna_sys_v1::ProviderAbiError::OperationRoleVersionMismatch,
    );

    println!(
        "generated_schema_typed_dispatch_relationship_parity schema_valid=3 typed_rejections=unknown_role,missing_reverse_link,role_version_mismatch total_cases=3"
    );
}

#[test]
fn generated_bindings_match_schema_valid_provider_edge_mutations() {
    fn effect_name(effect: SystemEffect) -> &'static str {
        match effect {
            SystemEffect::Read => "read",
            SystemEffect::Invoke => "invoke",
            SystemEffect::Admin => "admin",
        }
    }

    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let baseline_json = system_provider_abi_json();
    let registry: Value =
        serde_json::from_str(baseline_json).expect("embedded provider registry is valid JSON");
    build_host::validate_json_against_schema(baseline_json, &schema_json)
        .expect("embedded provider registry conforms to its generated schema");
    let table = system_dispatch_table();
    let mut version_schema_acceptances = 0;
    let mut effect_schema_acceptances = 0;
    let mut binding_cases = 0;

    for role in table.roles() {
        let version_mutation = {
            let mut mutated = registry.clone();
            let raw_role = mutated["roles"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|raw_role| raw_role["name"] == role.id.as_str())
                .expect("typed role is present in generated registry JSON");
            let minor = raw_role["version"]["minor"]
                .as_u64()
                .expect("generated role minor version is an integer");
            raw_role["version"]["minor"] = Value::from(minor + 1);
            mutated
        };
        let version_json = version_mutation.to_string();
        build_host::validate_json_against_schema(&version_json, &schema_json)
            .expect("well-formed version edge remains within the generated schema");
        assert_eq!(
            SystemProviderAbi::from_json(&version_json),
            Err(orna_sys_v1::ProviderAbiError::OperationRoleVersionMismatch),
            "typed dispatch rejects the version edge for {}",
            role.id.as_str()
        );
        version_schema_acceptances += 1;

        let role_operations = role
            .operations
            .iter()
            .map(|operation| {
                table
                    .operation(operation.as_str())
                    .expect("role edge resolves to typed operation")
            })
            .collect::<Vec<_>>();
        let incompatible_effect = [
            SystemEffect::Read,
            SystemEffect::Invoke,
            SystemEffect::Admin,
        ]
        .into_iter()
        .find(|candidate| {
            role_operations.iter().any(|operation| {
                !operation
                    .effects
                    .iter()
                    .all(|required| required == *candidate)
            })
        })
        .expect("a single valid effect cannot satisfy every operation on this role");
        let mut effect_mutation = registry.clone();
        let raw_role = effect_mutation["roles"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|raw_role| raw_role["name"] == role.id.as_str())
            .expect("typed role is present in generated registry JSON");
        raw_role["effects"] = serde_json::json!([effect_name(incompatible_effect)]);
        let effect_json = effect_mutation.to_string();
        build_host::validate_json_against_schema(&effect_json, &schema_json)
            .expect("well-formed effect edge remains within the generated schema");
        let incompatible = SystemProviderAbi::from_json(&effect_json)
            .expect("schema-valid effect edge deserializes before semantic validation");
        assert_eq!(
            incompatible.validate(),
            Err(orna_sys_v1::ProviderDiagnostic::EffectIncompatible(
                role.id.clone()
            )),
            "typed dispatch rejects the effect edge for {}",
            role.id.as_str()
        );
        effect_schema_acceptances += 1;

        for operation in role_operations {
            let generated =
                system_function_descriptor(operation.id.as_str()).unwrap_or_else(|| {
                    panic!("missing generated binding for {}", operation.id.as_str())
                });
            assert_eq!(generated.name, operation.id.as_str());
            assert_eq!(generated.signature, operation.signature.source);
            let expected_effect = operation
                .effects
                .iter()
                .next()
                .expect("generated provider operation has an effect");
            assert_eq!(expected_effect, generated.effect);
            let raw_operation = registry["operations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|raw_operation| raw_operation["name"] == operation.id.as_str())
                .expect("generated registry includes each provider operation");
            assert_eq!(
                raw_operation["signature"].as_str(),
                Some(generated.signature)
            );
            assert_eq!(
                raw_operation["effect"].as_str(),
                Some(effect_name(generated.effect))
            );
            let expected_role = format!(
                "{}@{}.{}",
                role.id.as_str(),
                role.version.major,
                role.version.minor
            );
            assert_eq!(raw_operation["role"].as_str(), Some(expected_role.as_str()));
            binding_cases += 1;
        }
    }

    assert_eq!(version_schema_acceptances, table.roles().count());
    assert_eq!(effect_schema_acceptances, table.roles().count());
    assert_eq!(
        binding_cases,
        table
            .operations()
            .filter(|operation| operation.role.is_some())
            .count()
    );
    println!(
        "generated_binding_schema_edge_parity roles={} schema_valid_edges=version,effect typed_edge_rejections=role_version_mismatch,effect_incompatible provider_bindings={binding_cases} total_cases={}",
        table.roles().count(),
        version_schema_acceptances + effect_schema_acceptances + binding_cases
    );
}

#[test]
fn generated_binding_provider_id_schema_rejections_match_typed_parser() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let registry: Value = serde_json::from_str(system_provider_abi_json())
        .expect("embedded provider registry is valid JSON");
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("embedded provider registry conforms to its generated schema");
    let table = system_dispatch_table();
    let mut role_id_rejections = 0;
    let mut provider_id_rejections = 0;
    let mut annotation_id_rejections = 0;
    let mut generated_bindings = 0;

    let assert_rejected = |mutated: Value, expected, label: &str| {
        let json = mutated.to_string();
        let schema_error = build_host::validate_json_against_schema(&json, &schema_json)
            .expect_err("generated provider schema rejects malformed identifiers");
        assert!(
            schema_error.contains("identifier pattern"),
            "schema rejection identifies {label}: {schema_error}"
        );
        assert_eq!(
            SystemProviderAbi::from_json(&json),
            Err(expected),
            "typed provider parser rejects the same malformed identifier in {label}"
        );
    };

    for role in table.roles() {
        let role_index = registry["roles"]
            .as_array()
            .unwrap()
            .iter()
            .position(|raw_role| raw_role["name"] == role.id.as_str())
            .expect("typed provider role appears in generated registry JSON");
        let mut invalid_role_id = registry.clone();
        invalid_role_id["roles"][role_index]["name"] = Value::String("invalid role id".to_owned());
        assert_rejected(
            invalid_role_id,
            orna_sys_v1::ProviderAbiError::InvalidRoleId,
            "role name",
        );
        role_id_rejections += 1;

        if role.builtin_provider.is_some() {
            let mut invalid_provider_id = registry.clone();
            invalid_provider_id["roles"][role_index]["builtin_provider"] =
                Value::String("invalid provider id".to_owned());
            assert_rejected(
                invalid_provider_id,
                orna_sys_v1::ProviderAbiError::InvalidProviderId,
                "built-in provider",
            );
            provider_id_rejections += 1;
        }

        for operation_id in &role.operations {
            let operation = table
                .operation(operation_id.as_str())
                .expect("provider role edge resolves to its typed operation");
            let generated =
                system_function_descriptor(operation.id.as_str()).unwrap_or_else(|| {
                    panic!("missing generated binding for {}", operation.id.as_str())
                });
            assert_eq!(generated.name, operation.id.as_str());
            assert_eq!(generated.signature, operation.signature.source);
            assert_eq!(
                operation.effects.iter().next(),
                Some(generated.effect),
                "generated binding effect matches {}",
                operation.id.as_str()
            );
            let operation_index = registry["operations"]
                .as_array()
                .unwrap()
                .iter()
                .position(|raw_operation| raw_operation["name"] == operation.id.as_str())
                .expect("generated registry operation row exists");
            let mut invalid_annotation = registry.clone();
            invalid_annotation["operations"][operation_index]["role"] = Value::String(format!(
                "invalid role id@{}.{}",
                role.version.major, role.version.minor
            ));
            assert_rejected(
                invalid_annotation,
                orna_sys_v1::ProviderAbiError::InvalidRoleId,
                "operation role annotation",
            );
            annotation_id_rejections += 1;
            generated_bindings += 1;
        }
    }

    assert_eq!(role_id_rejections, table.roles().count());
    assert_eq!(
        generated_bindings,
        table
            .operations()
            .filter(|operation| operation.role.is_some())
            .count()
    );
    assert_eq!(annotation_id_rejections, generated_bindings);
    println!(
        "generated_binding_provider_id_schema_parity roles={} role_id_rejections={role_id_rejections} provider_id_rejections={provider_id_rejections} operation_annotation_rejections={annotation_id_rejections} generated_bindings={generated_bindings} total_cases={}",
        table.roles().count(),
        role_id_rejections + provider_id_rejections + annotation_id_rejections
    );
}

#[test]
fn generated_provider_alias_id_edges_match_schema_and_binding_contracts() {
    const PROVIDER_ID_CASES: [(&str, bool); 12] = [
        ("-", true),
        ("_", true),
        ("9", true),
        ("_9.edge-name", true),
        ("orna.sys.v1", true),
        ("", false),
        (".", false),
        (".leading", false),
        ("trailing.", false),
        ("double..dot", false),
        ("white space", false),
        ("slash/name-é", false),
    ];

    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider alias schema regenerates from its source generator");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let registry: Value = serde_json::from_str(system_provider_abi_json())
        .expect("embedded provider alias registry is valid JSON");
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("embedded provider registry conforms to its generated schema");

    let alias_table = SystemDispatchTable::from_json(system_provider_abi_json())
        .expect("generated registry parses through the dispatch-table compatibility alias");
    let abi_table = SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("generated registry parses through the provider ABI name");
    assert_eq!(alias_table, abi_table);
    assert_eq!(&alias_table, system_dispatch_table());

    let mut generated_binding_cases = 0;
    for operation in alias_table.operations() {
        let generated = system_function_descriptor(operation.id.as_str())
            .unwrap_or_else(|| panic!("missing generated binding for {}", operation.id.as_str()));
        assert_eq!(generated.name, operation.id.as_str());
        assert_eq!(generated.signature, operation.signature.source);
        assert_eq!(
            operation.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect matches {}",
            operation.id.as_str()
        );
        generated_binding_cases += 1;
    }
    assert!(generated_binding_cases > 0);

    let role = registry["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|role| role["builtin_provider"].is_string())
        .expect("generated registry has a built-in provider alias");
    let role_name = role["name"]
        .as_str()
        .expect("built-in provider role has a name");
    let role_index = registry["roles"]
        .as_array()
        .unwrap()
        .iter()
        .position(|role| role["name"] == role_name)
        .expect("built-in provider role has a stable generated row");

    let mut schema_acceptances = 0;
    let mut schema_rejections = 0;
    for (provider_id, should_accept) in PROVIDER_ID_CASES {
        let mut mutated = registry.clone();
        mutated["roles"][role_index]["builtin_provider"] = Value::String(provider_id.to_owned());
        let json = mutated.to_string();
        let schema_result = build_host::validate_json_against_schema(&json, &schema_json);
        let alias_result = SystemDispatchTable::from_json(&json);
        let compatibility_result = SystemProviderAbi::from_json(&json);

        assert_eq!(
            schema_result.is_ok(),
            should_accept,
            "generated schema acceptance for provider alias {provider_id:?}"
        );
        assert_eq!(
            alias_result, compatibility_result,
            "dispatch and provider ABI aliases parse provider ID {provider_id:?} identically"
        );
        if should_accept {
            let parsed = alias_result.expect("schema-valid provider alias parses in typed table");
            assert_eq!(
                parsed
                    .role(role_name)
                    .and_then(|role| role.builtin_provider.as_ref())
                    .map(|provider| provider.as_str()),
                Some(provider_id),
                "typed dispatch preserves accepted provider alias {provider_id:?}"
            );
            schema_acceptances += 1;
        } else {
            assert_eq!(
                alias_result,
                Err(orna_sys_v1::ProviderAbiError::InvalidProviderId),
                "typed dispatch rejects provider alias {provider_id:?}"
            );
            schema_rejections += 1;
        }
    }

    println!(
        "generated_provider_alias_schema_parity valid_edges={schema_acceptances} invalid_edges={schema_rejections} generated_bindings={generated_binding_cases} total_cases={}",
        schema_acceptances + schema_rejections + generated_binding_cases
    );
}

#[test]
fn generated_schema_defers_semantic_dispatch_id_uniqueness_to_typed_registry() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch document");

    let mut duplicate_operation = registry.clone();
    let mut operation = duplicate_operation["operations"][0].clone();
    operation["preconditions"]
        .as_array_mut()
        .expect("operation preconditions are an array")
        .push(Value::String("sys.cont23.duplicate_probe".to_owned()));
    duplicate_operation["operations"]
        .as_array_mut()
        .unwrap()
        .push(operation);
    let duplicate_operation_json = duplicate_operation.to_string();
    build_host::validate_json_against_schema(&duplicate_operation_json, &schema_json)
        .expect("different operation rows satisfy schema uniqueItems");
    assert_eq!(
        SystemProviderAbi::from_json(&duplicate_operation_json),
        Err(orna_sys_v1::ProviderAbiError::DuplicateOperation),
        "typed dispatch keys operations by semantic name"
    );

    let mut duplicate_role = registry;
    let mut role = duplicate_role["roles"][0].clone();
    let replaceable = role["replaceable"]
        .as_bool()
        .expect("role replaceable flag is boolean");
    role["replaceable"] = Value::Bool(!replaceable);
    duplicate_role["roles"].as_array_mut().unwrap().push(role);
    let duplicate_role_json = duplicate_role.to_string();
    build_host::validate_json_against_schema(&duplicate_role_json, &schema_json)
        .expect("different role rows satisfy schema uniqueItems");
    assert_eq!(
        SystemProviderAbi::from_json(&duplicate_role_json),
        Err(orna_sys_v1::ProviderAbiError::DuplicateRole),
        "typed dispatch keys roles by semantic name"
    );

    println!(
        "generated_schema_typed_dispatch_identity_parity schema_valid=2 typed_rejections=duplicate_operation_id,duplicate_role_id total_cases=2"
    );
}

#[test]
fn generated_provider_schema_rejects_root_and_nested_registry_drift() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(
        schema_json,
        system_provider_abi_schema_json(),
        "the embedded provider schema matches fresh generated output"
    );
    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch document");
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("the embedded dispatch document passes its generated schema");

    let mut unknown_root_field = registry.clone();
    unknown_root_field["conformance_probe"] = Value::Bool(true);
    let mut unknown_operation_field = registry.clone();
    unknown_operation_field["operations"][0]["conformance_probe"] = Value::Bool(true);
    let mut unknown_role_field = registry.clone();
    unknown_role_field["roles"][0]["conformance_probe"] = Value::Bool(true);
    let mut unknown_version_field = registry.clone();
    unknown_version_field["abi_version"]["patch"] = Value::from(0);
    let mut missing_operation_field = registry;
    missing_operation_field["operations"][0]
        .as_object_mut()
        .unwrap()
        .remove("failures");

    let cases = [
        ("root unknown field", unknown_root_field, true, true),
        (
            "operation unknown field",
            unknown_operation_field,
            true,
            true,
        ),
        ("role unknown field", unknown_role_field, true, true),
        ("version unknown field", unknown_version_field, true, true),
        (
            "missing operation field",
            missing_operation_field,
            false,
            false,
        ),
    ];
    let mut schema_rejections = 0;
    let mut typed_unknown_field_acceptances = 0;
    for (label, mutated, unknown_field, typed_accepts) in cases {
        let json = mutated.to_string();
        let error = build_host::validate_json_against_schema(&json, &schema_json)
            .expect_err("provider schema drift must be rejected");
        let expected_error = if unknown_field {
            "unexpected field"
        } else {
            "missing required field"
        };
        assert!(
            error.contains(expected_error),
            "schema guard identifies {label}: {error}"
        );
        let typed_result = SystemProviderAbi::from_json(&json);
        assert_eq!(
            typed_result.is_ok(),
            typed_accepts,
            "typed deserializer behavior is explicit for {label}"
        );
        if typed_accepts {
            typed_unknown_field_acceptances += 1;
        } else {
            assert_eq!(
                typed_result,
                Err(orna_sys_v1::ProviderAbiError::InvalidJson),
                "missing raw fields remain invalid to typed deserialization"
            );
        }
        schema_rejections += 1;
    }

    assert_eq!(schema_rejections, 5);
    assert_eq!(typed_unknown_field_acceptances, 4);
    println!(
        "generated_provider_schema_drift_guard cases={schema_rejections} schema_rejections={schema_rejections} unknown_field_typed_acceptances={typed_unknown_field_acceptances} missing_required_typed_rejections=1 total_cases={}",
        schema_rejections + typed_unknown_field_acceptances
    );
}

#[test]
fn generated_provider_schema_drift_cannot_weaken_identifier_guard() {
    let regenerated = regenerate();
    let provider_schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(provider_schema_json, system_provider_abi_schema_json());
    let mut provider_schema: Value =
        serde_json::from_str(&provider_schema_json).expect("generated provider schema is JSON");
    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry JSON");
    build_host::validate_json_against_schema(system_provider_abi_json(), &provider_schema_json)
        .expect("embedded provider registry conforms to its generated schema");

    let mut malformed_registry = registry;
    malformed_registry["roles"][0]["name"] = Value::String("invalid role id".to_owned());
    let malformed_json = malformed_registry.to_string();
    assert!(
        build_host::validate_json_against_schema(&malformed_json, &provider_schema_json)
            .unwrap_err()
            .contains("identifier pattern"),
        "generated schema rejects an invalid provider role identifier"
    );
    assert_eq!(
        SystemProviderAbi::from_json(&malformed_json),
        Err(orna_sys_v1::ProviderAbiError::InvalidRoleId),
        "typed registry parser independently rejects the malformed role"
    );

    provider_schema["$defs"]["role"]["properties"]["name"]
        .as_object_mut()
        .expect("provider role name schema is an object")
        .remove("pattern")
        .expect("generated provider role names have an identifier pattern");
    let weakened_schema_json =
        build_support::canonical_pretty_json(&provider_schema).unwrap() + "\n";
    build_host::validate_json_against_schema(&malformed_json, &weakened_schema_json)
        .expect("weakened schema exposes why the generated identifier guard matters");
    assert_eq!(
        SystemProviderAbi::from_json(&malformed_json),
        Err(orna_sys_v1::ProviderAbiError::InvalidRoleId),
        "schema weakening does not alter the typed parser's rejection"
    );

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let host_registry_json = build_host::generate_host_registry(&source_root)
        .expect("host registry regenerates alongside the provider schema");
    let host_schema_json = build_host::generate_host_registry_schema()
        .expect("host operation schema regenerates alongside provider schema");
    let provider_schema_json = build_provider::generate_provider_registry_schema().unwrap();
    let out_dir = Path::new(env!("OUT_DIR"));
    let temp_root = std::env::temp_dir().join(format!(
        "orna-sys-provider-schema-drift-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&temp_root);
    copy_output_tree(out_dir, &temp_root).expect("copy generated artifacts for schema drift probe");
    fs::write(
        temp_root.join("system_provider_abi.schema.json"),
        &weakened_schema_json,
    )
    .expect("weaken only the copied generated provider schema");
    let drift = verify_generated_output_tree(
        &temp_root,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
        &provider_schema_json,
    )
    .expect_err("artifact parity rejects a weakened generated provider schema");
    assert!(
        drift.contains("system_provider_abi.schema.json"),
        "schema drift diagnostic names the generated schema: {drift}"
    );
    fs::remove_dir_all(&temp_root).expect("remove temporary schema drift artifacts");

    println!(
        "generated_provider_schema_semantic_drift original_schema_rejected=1 weakened_schema_acceptances=1 typed_parser_rejections=2 artifact_drift_rejections=1 total_cases=5"
    );
}

#[test]
fn generated_dispatch_metadata_rejects_provider_role_version_source_drift() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let collector = build_support::collect_rust_sources(&manifest.join("src"))
        .expect("annotated implementation methods form a valid provider registry");
    let registry = collector
        .type_graph
        .clone()
        .expect("annotated provider registry owns its type graph");
    let schema = collector
        .schema
        .clone()
        .expect("annotated provider registry owns its schema");
    let mut functions = collector.functions;
    let baseline = build_support::generate_sys_artifacts(&functions, registry.clone(), &schema)
        .expect("typed source generates provider dispatch metadata");
    assert_eq!(baseline.provider_abi_json, system_provider_abi_json());
    let provider_schema = build_provider::generate_provider_registry_schema()
        .expect("generated dispatch metadata schema is reproducible");
    build_host::validate_json_against_schema(&baseline.provider_abi_json, &provider_schema)
        .expect("generated dispatch metadata conforms to its schema");
    let parsed = SystemProviderAbi::from_json(&baseline.provider_abi_json)
        .expect("generated provider metadata parses into the runtime dispatch table");
    assert_eq!(&parsed, system_dispatch_table());

    let mut role_members = BTreeMap::<String, Vec<(usize, u16, u16)>>::new();
    for (index, function) in functions.iter().enumerate() {
        let Some(role) = &function.role else {
            continue;
        };
        let (name, major, minor) =
            build_support::parse_role_version(role).expect("source role has a parsed version");
        role_members
            .entry(name.to_owned())
            .or_default()
            .push((index, major, minor));
    }
    let (role_name, members) = role_members
        .into_iter()
        .find(|(_, members)| members.len() > 1)
        .expect("provider registry includes a role shared by multiple operations");
    let (changed_index, major, minor) = members[0];
    let changed_minor = minor
        .checked_add(1)
        .expect("source provider role minor version can be advanced for the drift probe");
    functions[changed_index].role = Some(format!("{role_name}@{major}.{changed_minor}"));

    let drift = build_support::generate_sys_artifacts(&functions, registry, &schema)
        .expect_err("inconsistent source role versions stop dispatch metadata generation");
    assert!(
        drift.contains(&format!(
            "semantic role `{role_name}` declares multiple versions"
        )),
        "generation diagnostic identifies the inconsistent provider role: {drift}"
    );
    println!(
        "generated_dispatch_metadata_source_validation baseline_parity=embedded,schema,runtime inconsistent_role_versions_rejected=1 total_cases=4"
    );
}

#[test]
fn generated_provider_schema_rejects_versions_outside_typed_u16_range() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let registry: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("embedded dispatch metadata conforms to its generated schema");
    let overflow = u64::from(u16::MAX) + 1;
    let mut overflow_cases = 0;
    let mut assert_rejected = |label: &str, mutated: Value, expected| {
        let json = mutated.to_string();
        assert!(
            build_host::validate_json_against_schema(&json, &schema_json).is_err(),
            "generated schema rejects overflowing {label}"
        );
        assert_eq!(
            SystemProviderAbi::from_json(&json),
            Err(expected),
            "typed provider registry rejects overflowing {label}"
        );
        overflow_cases += 1;
    };

    for field in ["major", "minor"] {
        let mut mutated = registry.clone();
        mutated["abi_version"][field] = Value::from(overflow);
        assert_rejected(
            &format!("abi_version.{field}"),
            mutated,
            orna_sys_v1::ProviderAbiError::InvalidJson,
        );

        let mut mutated = registry.clone();
        mutated["operations"][0]["version"][field] = Value::from(overflow);
        assert_rejected(
            &format!("operations[0].version.{field}"),
            mutated,
            orna_sys_v1::ProviderAbiError::InvalidJson,
        );

        let mut mutated = registry.clone();
        mutated["roles"][0]["version"][field] = Value::from(overflow);
        assert_rejected(
            &format!("roles[0].version.{field}"),
            mutated,
            orna_sys_v1::ProviderAbiError::InvalidJson,
        );
    }

    let operation = registry["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["role"].as_str().is_some())
        .expect("provider registry has a role-bound operation");
    let role_annotation = operation["role"].as_str().unwrap();
    let (role_name, version) = role_annotation
        .rsplit_once('@')
        .expect("role annotation includes its version");
    let (major, minor) = version
        .split_once('.')
        .expect("role annotation has major and minor versions");
    for (field, annotation) in [
        ("major", format!("{role_name}@{overflow}.{minor}")),
        ("minor", format!("{role_name}@{major}.{overflow}")),
    ] {
        let mut mutated = registry.clone();
        let operation_index = mutated["operations"]
            .as_array()
            .unwrap()
            .iter()
            .position(|candidate| candidate["role"].as_str() == Some(role_annotation))
            .expect("selected role-bound operation remains in the registry");
        mutated["operations"][operation_index]["role"] = Value::String(annotation);
        assert_rejected(
            &format!("operations[{operation_index}].role {field}"),
            mutated,
            orna_sys_v1::ProviderAbiError::InvalidRoleId,
        );
    }

    assert_eq!(overflow_cases, 8);
    println!(
        "generated_provider_version_overflow_schema_parity u16_max={} overflow={} numeric_version_fields=6 role_annotation_fields=2 schema_rejections={overflow_cases} typed_rejections={overflow_cases} total_cases={}",
        u16::MAX,
        overflow,
        overflow_cases * 2
    );
}

#[test]
fn generated_provider_schema_accepts_reordered_operation_and_role_rows() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider dispatch schema regenerates from its generator");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let mut reordered: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    let operation_count = reordered["operations"]
        .as_array()
        .expect("generated operations are an array")
        .len();
    let role_count = reordered["roles"]
        .as_array()
        .expect("generated roles are an array")
        .len();
    reordered["operations"]
        .as_array_mut()
        .expect("generated operations are an array")
        .reverse();
    reordered["roles"]
        .as_array_mut()
        .expect("generated roles are an array")
        .reverse();

    let reordered_json = reordered.to_string();
    build_host::validate_json_against_schema(&reordered_json, &schema_json)
        .expect("schema accepts valid operation and role rows in a different order");
    let reordered_table = SystemProviderAbi::from_json(&reordered_json)
        .expect("typed dispatch table accepts reordered operation and role rows");
    assert_eq!(&reordered_table, system_dispatch_table());
    assert_eq!(operation_count, reordered_table.operations().count());
    assert_eq!(role_count, reordered_table.roles().count());

    println!(
        "generated_provider_row_reorder_schema_parity operations={operation_count} roles={role_count} schema_valid=1 typed_table_equal=1 total_cases={}",
        operation_count + role_count + 2
    );
}

#[test]
fn generated_artifact_drift_probe_rejects_tampered_outputs_and_stale_modules() {
    let regenerated = regenerate();
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let host_registry_json = build_host::generate_host_registry(&source_root).unwrap();
    let host_schema_json = build_host::generate_host_registry_schema().unwrap();
    let provider_schema_json = build_provider::generate_provider_registry_schema().unwrap();
    let out_dir = Path::new(env!("OUT_DIR"));
    verify_generated_output_tree(
        out_dir,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
        &provider_schema_json,
    )
    .expect("baseline build outputs match their generated projections");

    let temp_root = std::env::temp_dir().join(format!(
        "orna-sys-drift-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&temp_root);
    fs::create_dir_all(&temp_root).expect("create drift-probe directory");

    let module_path = regenerated
        .binding_modules
        .keys()
        .next()
        .expect("sys registry emits at least one module")
        .clone();
    let cases = [
        ("api_sys.json", "api_sys.json".to_owned()),
        (
            "system_api_schema.json",
            "system_api_schema.json".to_owned(),
        ),
        (
            "system_provider_abi.json",
            "system_provider_abi.json".to_owned(),
        ),
        (
            "system_provider_abi.schema.json",
            "system_provider_abi.schema.json".to_owned(),
        ),
        (
            "system_binding_modules.json",
            "system_binding_modules.json".to_owned(),
        ),
        ("system_bindings.orna", "system_bindings.orna".to_owned()),
        (
            "system_host_operations.json",
            "system_host_operations.json".to_owned(),
        ),
        (
            "system_host_operations.schema.json",
            "system_host_operations.schema.json".to_owned(),
        ),
        ("generated module", format!("system_bindings/{module_path}")),
    ];

    for (index, (label, relative_path)) in cases.iter().enumerate() {
        let copy = temp_root.join(index.to_string());
        copy_output_tree(out_dir, &copy).expect("copy generated outputs for drift probe");
        let path = copy.join(relative_path);
        let mut bytes = fs::read(&path).expect("read copied generated artifact");
        bytes.extend_from_slice(b"\n// intentional drift probe\n");
        fs::write(&path, bytes).expect("tamper only with temporary generated artifact");
        let error = verify_generated_output_tree(
            &copy,
            &regenerated,
            &host_registry_json,
            &host_schema_json,
            &provider_schema_json,
        )
        .expect_err("drifted generated output must fail the parity guard");
        assert!(
            error.contains(relative_path) || error.contains(label),
            "the detector must identify {label}; got: {error}"
        );
    }

    let stale_copy = temp_root.join("stale-module");
    copy_output_tree(out_dir, &stale_copy).expect("copy outputs for stale-module probe");
    let stale_module = stale_copy.join("system_bindings/intentional_stale_module.orna");
    fs::write(&stale_module, "// stale generated module\n")
        .expect("add stale module only to temporary output tree");
    let error = verify_generated_output_tree(
        &stale_copy,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
        &provider_schema_json,
    )
    .expect_err("stale generated modules must fail the parity guard");
    assert!(error.contains("intentional_stale_module.orna"), "{error}");

    fs::remove_dir_all(&temp_root).expect("remove drift-probe directory");
}
