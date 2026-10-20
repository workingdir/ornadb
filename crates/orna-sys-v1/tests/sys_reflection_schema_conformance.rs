//! The reflection document must satisfy the generated sys JSON Schema through
//! the same fail-closed validator the build uses, and both must declare the
//! annotated-Blob codec code and the OVB-1 (legacy wire) / OVB-2 (new) profile
//! pair that the annotated-Blob boundary depends on.

use orna_sys_v1::{system_api_json, system_api_schema_json};
use serde_json::{Value, json};

#[path = "../src/abi_version.rs"]
mod abi_version;
#[path = "../src/host_registry_model.rs"]
mod host_registry_model;

#[path = "../build_host.rs"]
mod build_host;

const CODE: &str = "sys.codec.unsupported_value_profile";

fn reflection() -> Value {
    serde_json::from_str(&system_api_json()).expect("reflection JSON")
}

#[test]
fn reflection_document_validates_against_generated_schema() {
    build_host::validate_json_against_schema(&system_api_json(), system_api_schema_json())
        .expect("reflection must satisfy the generated sys schema");
}

#[test]
fn reflection_declares_codec_code_and_legacy_wire_profile_pair() {
    let document = reflection();
    let codes: Vec<&str> = document["failure_codes"]
        .as_array()
        .expect("failure_codes array")
        .iter()
        .map(|code| code.as_str().expect("failure code string"))
        .collect();
    assert!(codes.contains(&CODE), "reflection must declare {CODE}");
    assert_eq!(
        document["value_profiles"],
        json!({
            "new": "OVB-2",
            "stored": "ROV-3",
            "semantic": "SOV-3",
            "legacy_wire": "OVB-1"
        }),
        "value profiles must name the OVB-1 legacy wire and OVB-2 new profile"
    );
}

#[test]
fn schema_rejects_reflection_that_drops_the_legacy_wire_profile() {
    let mut document = reflection();
    document["value_profiles"]
        .as_object_mut()
        .expect("value_profiles object")
        .remove("legacy_wire");
    assert!(
        build_host::validate_json_against_schema(&document.to_string(), system_api_schema_json())
            .is_err(),
        "a reflection missing value_profiles.legacy_wire must fail schema validation"
    );
}

#[test]
fn schema_rejects_reflection_with_blank_failure_code() {
    let mut document = reflection();
    document["failure_codes"]
        .as_array_mut()
        .expect("failure_codes array")
        .push(json!(""));
    assert!(
        build_host::validate_json_against_schema(&document.to_string(), system_api_schema_json())
            .is_err(),
        "a blank failure code must fail schema validation"
    );
}
