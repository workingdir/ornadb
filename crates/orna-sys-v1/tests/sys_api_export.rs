#![cfg(feature = "dev-sys-export")]

use std::{fs, process::Command};

use orna_sys_v1::{system_api_json, system_api_schema_json};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const SYS_API_V1_SHA256: &str = "b569785bfaa204b366b2cee444c01a9aa8dd74c710852fdad925dcfae60a256f";

#[test]
fn dev_export_writes_the_embedded_schema_validated_contract_byte_for_byte() {
    let output_path = std::env::temp_dir().join(format!("orna-sys-api-{}.json", std::process::id()));
    let _ = fs::remove_file(&output_path);
    let output = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .arg(&output_path)
        .output()
        .expect("run dev-only sys API exporter");
    assert!(
        output.status.success(),
        "exporter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = system_api_json();
    let exported = fs::read(&output_path).expect("read exported sys API artifact");
    fs::remove_file(&output_path).expect("remove temporary sys API export");
    assert_eq!(exported, expected.as_bytes());
    assert_eq!(
        format!("{:x}", Sha256::digest(&exported)),
        SYS_API_V1_SHA256,
        "the export preserves the frozen Orna 1.0.0 contract bytes"
    );

    let api: Value = serde_json::from_slice(&exported).expect("exported API JSON");
    let schema: Value =
        serde_json::from_str(system_api_schema_json()).expect("embedded generated schema");
    build_support::validate_published_schema_shape(&api, &schema)
        .expect("exported contract matches the embedded schema");
    build_support::validate_api_document(&api)
        .expect("exported contract satisfies the 1.0 type graph and schema invariants");

    let stdout = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .output()
        .expect("run stdout export");
    assert!(stdout.status.success());
    assert_eq!(stdout.stdout, exported, "stdout export is byte-stable");
}
