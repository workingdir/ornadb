use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

mod build_support;

use build_support::{
    Collector, canonical_pretty_json, generate_binding_stubs, parse_role_version,
    validate_api_document, validate_collection,
};
use serde_json::{Value, json};

fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
    Ok(())
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("orna-sys-v1 must be two levels below the workspace root");
    let source_root = manifest.join("src");
    let api_path = workspace.join("api/sys.json");
    let schema_path = workspace.join("api/sys.schema.json");
    let build_support_path = manifest.join("build_support.rs");
    println!("cargo:rerun-if-changed={}", api_path.display());
    println!("cargo:rerun-if-changed={}", schema_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", build_support_path.display());
    // Watch the directory recursively so adding a new annotated module also
    // invalidates the collected schema, even before that file is known here.
    println!("cargo:rerun-if-changed={}", source_root.display());

    // `syn` scans written Rust source and cannot see items emitted later by
    // `macro_rules!` or procedural-macro expansion. Annotated methods in impls
    // and trait declarations are collected; methods emitted by macros remain
    // unsupported and are documented at the source declaration.
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources).expect("walk sys crate Rust source");
    let mut collector = Collector::default();
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
        let text = fs::read_to_string(source)
            .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        collector.collect_source(&source.display().to_string(), &text);
    }
    assert!(collector.errors.is_empty(), "{}", collector.errors.join("\n"));
    assert!(
        !collector.functions.is_empty(),
        "no #[ornasys] trait or implementation methods were collected"
    );
    validate_collection(&collector.functions)
        .expect("annotated system API collection must be unique and internally consistent");

    let schema_text = fs::read_to_string(&schema_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", schema_path.display()));
    let schema: Value = serde_json::from_str(&schema_text).expect("valid published JSON Schema");
    assert_eq!(
        schema["$schema"].as_str(),
        Some("https://json-schema.org/draft/2020-12/schema"),
        "published system API schema must use JSON Schema 2020-12"
    );
    assert_eq!(schema["type"].as_str(), Some("object"));

    // The normative API artifact owns the non-function type/relation graph;
    // function declarations are replaced by the build-time method collection.
    let base_text = fs::read_to_string(&api_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", api_path.display()));
    let mut api: Value = serde_json::from_str(&base_text).expect("valid normative system API JSON");
    let functions = collector
        .functions
        .iter()
        .map(|function| function.metadata.clone())
        .collect::<Vec<_>>();
    api["functions"] = Value::Array(functions);
    api["counts"]["functions"] = json!(collector.functions.len());
    api["source_of_truth"] = json!(
        "crates/orna-sys-v1/src/system_api.rs #[ornasys] descriptor methods; normative semantics in source chapters and generated system reference"
    );

    validate_api_document(&api).expect("generated system API schema is internally consistent");

    let mut generated_json = canonical_pretty_json(&api).expect("serialize system API canonically");
    generated_json.push('\n');

    // Project the same annotated methods to an internal typed provider ABI.
    // Role/version metadata stays out of the frozen public 1.0 artifact.
    let declared_failures = api["failure_codes"]
        .as_array()
        .expect("validated API failure-code array");
    let operations = collector
        .functions
        .iter()
        .map(|function| {
            let name = function.metadata["name"].as_str().expect("validated name");
            let operation_namespace = name
                .find(['(', '<'])
                .map_or(name, |end| &name[..end]);
            let failures = declared_failures
                .iter()
                .filter_map(Value::as_str)
                .filter(|code| {
                    *code == operation_namespace
                        || code
                            .strip_prefix(operation_namespace)
                            .is_some_and(|tail| tail.starts_with('.'))
                })
                .collect::<Vec<_>>();
            let preconditions = function.metadata["preconditions"]
                .as_str()
                .map(|value| vec![value])
                .unwrap_or_default();
            json!({
                "name": name,
                "version": {"major": 1, "minor": 0},
                "signature": function.metadata["signature"],
                "effect": function.metadata["effect"],
                "preconditions": preconditions,
                "failures": failures,
                "role": function.role,
            })
        })
        .collect::<Vec<_>>();
    let mut roles = std::collections::BTreeMap::<String, Value>::new();
    for function in &collector.functions {
        let Some(role) = &function.role else {
            continue;
        };
        let (name, major, minor) = parse_role_version(role).expect("role validated by collector");
        let effect = function.metadata["effect"].as_str().expect("validated effect");
        let role_entry = roles.entry(name.to_owned()).or_insert_with(|| {
            json!({
                "name": name,
                "version": {"major": major, "minor": minor},
                "effects": [],
                "operations": [],
                "required": true,
                "replaceable": false,
                "builtin_provider": "orna.sys.v1",
            })
        });
        role_entry["effects"]
            .as_array_mut()
            .expect("generated effect array")
            .push(json!(effect));
        role_entry["operations"]
            .as_array_mut()
            .expect("generated role operation list")
            .push(function.metadata["name"].clone());
    }
    for role in roles.values_mut() {
        let effects = role["effects"]
            .as_array()
            .expect("generated role effects")
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(effects.len(), 1, "semantic role has incompatible effects");
        role["effects"] = json!(effects);
        let operations = role["operations"]
            .as_array()
            .expect("generated role operation list")
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        role["operations"] = json!(operations);
    }
    let provider_abi = json!({
        "abi_version": {"major": 1, "minor": 0},
        "operations": operations,
        "roles": roles.into_values().collect::<Vec<_>>(),
    });
    let mut provider_abi_json =
        canonical_pretty_json(&provider_abi).expect("serialize provider ABI canonically");
    provider_abi_json.push('\n');
    let generated_binding_stubs = generate_binding_stubs(
        provider_abi["operations"]
            .as_array()
            .expect("generated typed provider operations"),
    )
    .expect("registered sys signatures must be supported by the Orna stub emitter");
    let mut binding_modules = BTreeMap::<String, String>::new();
    let mut binding_bundle = String::from(
        "// Generated sys binding declaration bundle. Module markers identify the emitted .orna file.\n",
    );
    for stub in generated_binding_stubs {
        let relative_path = format!("{}.orna", stub.module.replace('.', "/"));
        let module_source = binding_modules.entry(relative_path).or_insert_with(|| {
            format!(
                "// Generated built-in module `{}` from the typed sys provider registry.\n// Orna keyword parameter aliases are suffixed with `_`; comments preserve registered names.\n",
                stub.module
            )
        });
        module_source.push_str(&stub.source);
        binding_bundle.push_str(&format!("// sys-module: {}\n", stub.module));
        binding_bundle.push_str(&stub.source);
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out_dir.join("api_sys.json"), generated_json).expect("write generated api/sys.json");
    fs::write(out_dir.join("system_provider_abi.json"), provider_abi_json)
        .expect("write generated typed system provider ABI");
    let binding_root = out_dir.join("system_bindings");
    if binding_root.exists() {
        fs::remove_dir_all(&binding_root).expect("remove stale generated sys binding stubs");
    }
    for (relative_path, source) in binding_modules {
        let path = binding_root.join(relative_path);
        fs::create_dir_all(path.parent().expect("generated module has a parent"))
            .expect("create generated sys module directory");
        fs::write(path, source).expect("write generated Orna sys module stub");
    }
    fs::write(out_dir.join("system_bindings.orna"), binding_bundle)
        .expect("write generated Orna sys binding stubs");
}
