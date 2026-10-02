use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use orna_sys_v1::{
    SystemProviderAbi, system_api_json, system_api_schema_json, system_binding_stubs,
    system_dispatch_table, system_host_operation_registry_json,
    system_host_operation_registry_schema_json, system_provider_abi_json,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
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
) -> Result<(), String> {
    for (relative_path, expected) in [
        ("api_sys.json", artifacts.api_json.as_str()),
        ("system_api_schema.json", artifacts.schema_json.as_str()),
        (
            "system_provider_abi.json",
            artifacts.provider_abi_json.as_str(),
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
fn generated_sys_artifacts_regenerate_byte_for_byte_across_fresh_registry_builds() {
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
    let host_schema_json = build_host::generate_host_registry_schema()
        .expect("native host operation schema regenerates deterministically");

    assert_eq!(regenerated.api_json, system_api_json());
    let api_hash = format!("{:x}", Sha256::digest(regenerated.api_json.as_bytes()));
    assert_eq!(
        api_hash, SYS_API_V1_SHA256,
        "on-demand API retains the frozen 1.0 bytes"
    );
    assert_eq!(regenerated.schema_json, system_api_schema_json());
    assert_eq!(
        regenerated.provider_abi_json,
        system_provider_abi_json(),
        "embedded dispatch table JSON must match a fresh typed-registry projection"
    );
    assert_eq!(
        regenerated.binding_bundle,
        system_binding_stubs(),
        "embedded Orna declaration bundle must match a fresh typed-registry projection"
    );

    assert_eq!(
        fs::read_to_string(out_dir.join("api_sys.json")).expect("read build API artifact"),
        regenerated.api_json
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("system_api_schema.json"))
            .expect("read build schema artifact"),
        regenerated.schema_json
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("system_provider_abi.json"))
            .expect("read build dispatch artifact"),
        regenerated.provider_abi_json
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("system_bindings.orna"))
            .expect("read build binding bundle"),
        regenerated.binding_bundle
    );
    assert_eq!(
        system_host_operation_registry_json(),
        host_registry_json,
        "embedded host dispatch metadata matches a fresh annotated-method projection"
    );
    assert_eq!(
        system_host_operation_registry_schema_json(),
        host_schema_json,
        "embedded host dispatch schema matches a fresh generated schema"
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("system_host_operations.json"))
            .expect("read build host operation artifact"),
        host_registry_json
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("system_host_operations.schema.json"))
            .expect("read build host operation schema artifact"),
        host_schema_json
    );
    verify_generated_output_tree(
        out_dir,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
    )
    .expect("every build output exactly matches fresh typed-registry projections");

    let regenerated_table = SystemProviderAbi::from_json(&regenerated.provider_abi_json)
        .expect("regenerated dispatch table is valid");
    assert_eq!(system_dispatch_table(), &regenerated_table);
    let api: Value = serde_json::from_str(&regenerated.api_json).expect("regenerated API JSON");
    let schema: Value =
        serde_json::from_str(&regenerated.schema_json).expect("generated system API schema");
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
fn generated_artifact_drift_probe_rejects_tampered_outputs_and_stale_modules() {
    let regenerated = regenerate();
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let host_registry_json = build_host::generate_host_registry(&source_root).unwrap();
    let host_schema_json = build_host::generate_host_registry_schema().unwrap();
    let out_dir = Path::new(env!("OUT_DIR"));
    verify_generated_output_tree(
        out_dir,
        &regenerated,
        &host_registry_json,
        &host_schema_json,
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
    )
    .expect_err("stale generated modules must fail the parity guard");
    assert!(error.contains("intentional_stale_module.orna"), "{error}");

    fs::remove_dir_all(&temp_root).expect("remove drift-probe directory");
}
