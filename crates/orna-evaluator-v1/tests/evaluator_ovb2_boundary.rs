use std::collections::BTreeMap;

use orna_evaluator_v1::{Environment, Limits, evaluate_expression, evaluate_expression_ovb2};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Blob, ContextValue, Raw, ValueFormat};

fn annotated_blob_value(bytes: &[u8]) -> ContextValue {
    let blob = Blob::from_bytes_with_annotation(bytes.to_vec(), "image/jpeg", Some("jpeg"))
        .expect("valid MIME type and suffix");
    ContextValue::from_blob(&blob, ValueFormat::Ovb2).expect("encode OVB-2 Blob value")
}

#[test]
fn ovb2_evaluator_preserves_annotated_blob_at_top_level_and_in_a_list() {
    let bytes = [0xff, 0xd8, 0x00, 0xff, 0xd9];
    let blob_value = annotated_blob_value(&bytes);
    let environment = BTreeMap::from([("asset".to_owned(), blob_value.clone())]);

    let returned = evaluate_expression_ovb2("asset", &environment, Limits::default())
        .expect("evaluate OVB-2 Blob variable");
    let returned_blob = returned.blob().expect("returned annotated Blob");
    assert_eq!(returned.format(), ValueFormat::Ovb2);
    assert_eq!(returned_blob.media_type(), "image/jpeg");
    assert_eq!(returned_blob.suffix(), Some("jpeg"));
    assert_eq!(returned_blob.read_to_end().expect("read Blob bytes"), bytes);

    let list = ContextValue::new(
        ValueFormat::Ovb2,
        Raw::Array(vec![blob_value.raw().clone()]),
    )
    .expect("valid OVB-2 list containing Blob");
    let environment = BTreeMap::from([("assets".to_owned(), list)]);
    let returned = evaluate_expression_ovb2("assets", &environment, Limits::default())
        .expect("evaluate OVB-2 list variable");
    let Raw::Array(values) = returned.raw() else {
        panic!("expected returned OVB-2 list");
    };
    let nested_blob = ContextValue::new(ValueFormat::Ovb2, values[0].clone())
        .expect("returned nested OVB-2 Blob");
    let nested_blob = nested_blob.blob().expect("nested annotated Blob");
    assert_eq!(nested_blob.media_type(), "image/jpeg");
    assert_eq!(nested_blob.suffix(), Some("jpeg"));
    assert_eq!(
        nested_blob.read_to_end().expect("read nested Blob bytes"),
        bytes
    );
}

#[test]
fn legacy_ovb1_evaluator_still_treats_blob_bytes_as_bytes() {
    let bytes = vec![0xff, 0xd8, 0x00, 0xff, 0xd9];
    let value = CanonicalValue::new(Raw::Bytes(bytes.clone())).expect("canonical OVB-1 bytes");
    let environment: Environment = BTreeMap::from([("asset".to_owned(), value.clone())]);

    assert_eq!(
        evaluate_expression("asset", &environment, Limits::default())
            .expect("evaluate legacy OVB-1 byte value"),
        value
    );
}

#[test]
fn ovb2_relation_payload_is_rejected_as_unsupported() {
    let object_id = |byte| Raw::Tag(37, Box::new(Raw::Bytes(vec![byte; 16])));
    let relation = ContextValue::new(
        ValueFormat::Ovb2,
        Raw::Tag(
            60021,
            Box::new(Raw::Array(vec![
                object_id(1),
                object_id(2),
                Raw::Int(7.into()),
            ])),
        ),
    )
    .expect("valid relation context value");
    let environment = BTreeMap::from([("relation".to_owned(), relation)]);

    let failure = evaluate_expression_ovb2("relation", &environment, Limits::default())
        .expect_err("relation payload is outside the evaluator value subset");
    assert_eq!(failure.code(), "ORNA-EVAL-UNSUPPORTED");
}
