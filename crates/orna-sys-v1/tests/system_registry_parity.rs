use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use orna_sys_v1::{
    SystemProviderAbi, system_api_json, system_api_schema_json, system_binding_stubs,
    system_dispatch_table, system_provider_abi_json,
};
use sha2::{Digest, Sha256};
use serde_json::Value;

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

fn generated_modules(root: &Path) -> BTreeMap<String, String> {
    fn visit(root: &Path, base: &Path, files: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(root).expect("read generated binding directory") {
            let path = entry.expect("read generated binding entry").path();
            if path.is_dir() {
                visit(&path, base, files);
            } else if path.extension().is_some_and(|extension| extension == "orna") {
                let key = path
                    .strip_prefix(base)
                    .expect("generated file is under binding root")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(
                    key,
                    fs::read_to_string(path).expect("read generated module stub"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn generated_sys_artifacts_regenerate_byte_for_byte_from_annotated_registry() {
    let regenerated = regenerate();
    let out_dir = Path::new(env!("OUT_DIR"));

    assert_eq!(regenerated.api_json, system_api_json());
    let api_hash = format!("{:x}", Sha256::digest(regenerated.api_json.as_bytes()));
    assert_eq!(api_hash, SYS_API_V1_SHA256, "on-demand API retains the frozen 1.0 bytes");
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
        generated_modules(&out_dir.join("system_bindings")),
        regenerated.binding_modules,
        "each emitted .orna module must match a fresh registry projection"
    );

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
    assert_eq!(function_ids, dispatch_ids, "API and dispatch registry are 1:1");
}
