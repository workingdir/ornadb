//! Store-format-3 Blob annotation clause: an annotated Blob crossing the
//! legacy OVB-1 interface is an explicit error with the code
//! `sys.codec.unsupported_value_profile`, and never a silent loss of the
//! annotation. The code must be declared by the generated schema and
//! reflection output, and produced identically by the runtime.

use std::collections::BTreeMap;

use orna_evaluator_v1::{Limits, evaluate_expression_ovb2};
use orna_sys_v1::{system_api_json, system_api_schema_json};
use orna_value_v1::{Blob, ContextValue, Raw, ValueFormat};
use serde_json::Value;

const CODE: &str = "sys.codec.unsupported_value_profile";
const ANNOTATE: &str = r#"sys.blob.annotate(asset, "image/jpeg", "jpeg")"#;

fn failure_codes(document: &Value) -> Vec<&str> {
    document["failure_codes"]
        .as_array()
        .expect("failure_codes array")
        .iter()
        .map(|code| code.as_str().expect("failure code string"))
        .collect()
}

#[test]
fn schema_surface_declares_codec_code_and_legacy_wire_profile() {
    let schema: Value = serde_json::from_str(system_api_schema_json()).expect("schema JSON");
    assert!(schema["properties"]["failure_codes"].is_object());
    assert!(schema["properties"]["value_profiles"].is_object());
    let schema_text = system_api_schema_json();
    assert!(
        schema_text.contains("failure_codes") && schema_text.contains("value_profiles"),
        "generated schema must describe the codec failure and profile surface"
    );
}

#[test]
fn reflection_surface_declares_codec_code_and_legacy_wire_profile() {
    let document: Value = serde_json::from_str(&system_api_json()).expect("reflection JSON");
    assert!(
        failure_codes(&document).contains(&CODE),
        "reflection must declare {CODE}"
    );
    assert_eq!(document["value_profiles"]["legacy_wire"], "OVB-1");
    assert_eq!(document["value_profiles"]["new"], "OVB-2");
}

#[test]
fn runtime_surface_rejects_legacy_ovb1_annotation_with_declared_code() {
    let legacy = ContextValue::new(ValueFormat::Ovb1, Raw::Bytes(b"payload".to_vec()))
        .expect("canonical OVB-1 bytes");
    let environment = BTreeMap::from([("asset".to_owned(), legacy)]);
    let error = evaluate_expression_ovb2(ANNOTATE, &environment, Limits::default())
        .expect_err("OVB-1 source must not satisfy the annotated Blob interface");
    assert_eq!(error.code(), CODE);

    let declared: Value = serde_json::from_str(&system_api_json()).expect("reflection JSON");
    assert!(
        failure_codes(&declared).contains(&error.code()),
        "runtime code must be the one reflection declares"
    );
}

#[test]
fn runtime_annotation_is_identical_through_ovb2_for_same_call() {
    let bytes = b"payload".to_vec();
    let blob = Blob::from_bytes_with_annotation(bytes.clone(), "image/jpeg", Some("jpeg"))
        .expect("valid annotation");
    let original = blob.content_identity();
    let value = ContextValue::from_blob(&blob, ValueFormat::Ovb2).expect("OVB-2 Blob");
    let environment = BTreeMap::from([("asset".to_owned(), value)]);
    let annotated = evaluate_expression_ovb2(ANNOTATE, &environment, Limits::default())
        .expect("OVB-2 annotation succeeds for the same call");
    let annotated_blob = annotated.blob().expect("annotated OVB-2 Blob");
    assert_eq!(annotated_blob.media_type(), "image/jpeg");
    assert_eq!(annotated_blob.content_identity(), original);
    assert_eq!(annotated_blob.read_to_end().expect("read Blob"), bytes);
}

#[test]
fn export_surface_refuses_annotated_blob_in_ovb1_instead_of_dropping_annotation() {
    let blob = Blob::from_bytes_with_annotation(b"payload".to_vec(), "image/jpeg", Some("jpeg"))
        .expect("valid annotation");
    assert!(
        ContextValue::from_blob(&blob, ValueFormat::Ovb1).is_err(),
        "OVB-1 export must refuse an annotated Blob"
    );
}
