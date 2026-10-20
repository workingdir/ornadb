//! Store-format-3 Blob annotation clause: an annotated Blob crossing the
//! legacy OVB-1 interface is an explicit error with the code
//! `sys.codec.unsupported_value_profile`, and never a silent loss of the
//! annotation. Schema and reflection conformance lives in
//! `orna-sys-v1/tests/sys_reflection_schema_conformance.rs`.
//!
//! Library-layer coverage only: no CLI route accepts OVB-1 values as input
//! (see gap bead for the missing CLI/export harness).

use std::collections::BTreeMap;

use orna_evaluator_v1::{Limits, evaluate_expression_ovb2};
use orna_sys_v1::system_api_json;
use orna_value_v1::{Blob, ContextValue, Error as ValueError, Raw, ValueFormat};
use serde_json::Value;

const CODE: &str = "sys.codec.unsupported_value_profile";
const ANNOTATE: &str = r#"sys.blob.annotate(asset, "image/jpeg", "jpeg")"#;

#[test]
fn runtime_rejects_legacy_ovb1_annotation_with_declared_code() {
    let legacy = ContextValue::new(ValueFormat::Ovb1, Raw::Bytes(b"payload".to_vec()))
        .expect("canonical OVB-1 bytes");
    let environment = BTreeMap::from([("asset".to_owned(), legacy)]);
    let error = evaluate_expression_ovb2(ANNOTATE, &environment, Limits::default())
        .expect_err("OVB-1 source must not satisfy the annotated Blob interface");
    assert_eq!(error.code(), CODE);

    let declared: Value = serde_json::from_str(&system_api_json()).expect("reflection JSON");
    let codes: Vec<&str> = declared["failure_codes"]
        .as_array()
        .expect("failure_codes array")
        .iter()
        .map(|code| code.as_str().expect("failure code string"))
        .collect();
    assert!(
        codes.contains(&error.code()),
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

/// Value-layer export refusal. This is the only OVB-1 export boundary exercised
/// here; it returns `InvalidProfile` and does not map to the sys code above.
#[test]
fn value_layer_ovb1_export_refuses_annotated_blob_with_invalid_profile() {
    let blob = Blob::from_bytes_with_annotation(b"payload".to_vec(), "image/jpeg", Some("jpeg"))
        .expect("valid annotation");
    assert_eq!(
        ContextValue::from_blob(&blob, ValueFormat::Ovb1).err(),
        Some(ValueError::InvalidProfile),
        "OVB-1 export must refuse an annotated Blob with InvalidProfile"
    );
}
