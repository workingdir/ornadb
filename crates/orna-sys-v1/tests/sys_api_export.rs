#![cfg(feature = "dev-sys-export")]

use std::{fs, process::Command};

use orna_sys_v1::{
    SystemProviderAbi, system_api_json, system_api_schema_json, system_dispatch_table,
    system_provider_abi_json, system_provider_abi_schema_json,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const SYS_API_V1_SHA256: &str = "b569785bfaa204b366b2cee444c01a9aa8dd74c710852fdad925dcfae60a256f";

fn export(schema: bool, output_path: Option<&std::path::Path>) -> Vec<u8> {
    export_mode(schema.then_some("--schema"), output_path)
}

fn export_mode(option: Option<&str>, output_path: Option<&std::path::Path>) -> Vec<u8> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sys-api-export"));
    if let Some(option) = option {
        command.arg(option);
    }
    if let Some(output_path) = output_path {
        command.arg(output_path);
    }
    let output = command
        .output()
        .expect("run dev-only sys artifact exporter");
    assert!(
        output.status.success(),
        "exporter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    match output_path {
        Some(path) => fs::read(path).expect("read exported sys artifact"),
        None => output.stdout,
    }
}

#[test]
fn dev_exports_are_byte_stable_and_api_export_matches_the_embedded_schema() {
    let output_dir =
        std::env::temp_dir().join(format!("orna-sys-artifacts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).expect("create temporary export directory");
    let api_path_one = output_dir.join("first/api.json");
    let api_path_two = output_dir.join("second/api.json");
    let exported = export(false, Some(&api_path_one));
    let second_export = export(false, Some(&api_path_two));
    assert_eq!(
        exported, second_export,
        "separate exporter runs are byte-stable"
    );
    assert_eq!(exported, system_api_json().as_bytes());
    assert_eq!(
        format!("{:x}", Sha256::digest(&exported)),
        SYS_API_V1_SHA256,
        "the export preserves the frozen Orna 1.0.0 contract bytes"
    );

    let api: Value = serde_json::from_slice(&exported).expect("exported API JSON");
    let schema: Value =
        serde_json::from_str(system_api_schema_json()).expect("embedded generated schema");
    let canonical_schema = build_support::canonical_pretty_json(&schema)
        .expect("canonical embedded schema JSON")
        + "\n";
    assert_eq!(
        system_api_schema_json(),
        canonical_schema,
        "the embedded schema uses deterministic canonical serialization"
    );
    build_support::validate_published_schema_shape(&api, &schema)
        .expect("exported contract matches the embedded schema");
    build_support::validate_api_document(&api)
        .expect("exported contract satisfies the 1.0 type graph and schema invariants");

    assert_eq!(
        export(false, None),
        exported,
        "first stdout export matches the file"
    );
    assert_eq!(
        export(false, None),
        exported,
        "second stdout export is byte-stable"
    );

    let schema_path_one = output_dir.join("first/schema.json");
    let schema_path_two = output_dir.join("second/schema.json");
    let exported_schema = export(true, Some(&schema_path_one));
    assert_eq!(exported_schema, system_api_schema_json().as_bytes());
    assert_eq!(
        exported_schema,
        export(true, Some(&schema_path_two)),
        "separate schema exporter runs are byte-stable"
    );
    assert_eq!(export(true, None), exported_schema);
    assert_eq!(export(true, None), exported_schema);

    fs::remove_dir_all(&output_dir).expect("remove temporary export directory");
}

#[test]
fn dev_provider_registry_export_is_byte_stable_schema_valid_and_runtime_identical() {
    let output_dir = std::env::temp_dir().join(format!(
        "orna-sys-provider-artifacts-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).expect("create temporary provider export directory");
    let first_path = output_dir.join("first/dispatch.json");
    let second_path = output_dir.join("second/dispatch.json");
    let exported = export_mode(Some("--provider-abi"), Some(&first_path));
    assert_eq!(
        exported,
        export_mode(Some("--provider-abi"), Some(&second_path)),
        "provider registry exports from separate processes are byte-stable"
    );
    assert_eq!(exported, system_provider_abi_json().as_bytes());
    let registry: Value = serde_json::from_slice(&exported).expect("exported provider registry");
    assert_eq!(
        build_support::canonical_pretty_json(&registry).expect("canonical provider registry")
            + "\n",
        std::str::from_utf8(&exported).expect("provider export is UTF-8"),
        "provider registry export uses deterministic canonical serialization"
    );

    let first_schema_path = output_dir.join("first/dispatch.schema.json");
    let second_schema_path = output_dir.join("second/dispatch.schema.json");
    let exported_schema = export_mode(Some("--provider-abi-schema"), Some(&first_schema_path));
    assert_eq!(
        exported_schema,
        export_mode(Some("--provider-abi-schema"), Some(&second_schema_path)),
        "provider schema exports from separate processes are byte-stable"
    );
    assert_eq!(
        exported_schema,
        system_provider_abi_schema_json().as_bytes()
    );

    let registry_json = std::str::from_utf8(&exported).expect("provider export is UTF-8");
    let schema_json = std::str::from_utf8(&exported_schema).expect("provider schema is UTF-8");
    build_host::validate_json_against_schema(registry_json, schema_json)
        .expect("conformance export matches its embedded provider schema");
    let exported_table = SystemProviderAbi::from_json(registry_json)
        .expect("conformance export deserializes into the typed provider registry");
    assert_eq!(
        &exported_table,
        system_dispatch_table(),
        "conformance export resolves to the exact runtime dispatch table"
    );
    let schema: Value = serde_json::from_str(schema_json).expect("exported provider schema JSON");
    assert_eq!(
        build_support::canonical_pretty_json(&schema).expect("canonical provider schema") + "\n",
        schema_json,
        "provider schema export is canonically serialized"
    );
    assert_eq!(export_mode(Some("--provider-abi"), None), exported);
    assert_eq!(
        export_mode(Some("--provider-abi-schema"), None),
        exported_schema
    );

    fs::remove_dir_all(&output_dir).expect("remove temporary provider export directory");
}
