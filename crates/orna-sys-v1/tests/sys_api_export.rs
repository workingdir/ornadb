#![cfg(feature = "dev-sys-export")]

use std::{fs, process::Command};

use orna_sys_v1::{system_api_json, system_api_schema_json};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const SYS_API_V1_SHA256: &str = "b569785bfaa204b366b2cee444c01a9aa8dd74c710852fdad925dcfae60a256f";

fn export(schema: bool, output_path: Option<&std::path::Path>) -> Vec<u8> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sys-api-export"));
    if schema {
        command.arg("--schema");
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
