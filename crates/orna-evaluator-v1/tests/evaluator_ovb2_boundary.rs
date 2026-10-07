use std::collections::BTreeMap;

use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Limits, Ovb2EffectHandler, evaluate_expression,
    evaluate_expression_ovb2, evaluate_expression_ovb2_with_effects,
    evaluate_expression_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::Expr;
use orna_value_v1::{Blob, ContextValue, OVB2_BLOB_TAG, Raw, ValueFormat};

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
fn native_sys_blob_annotate_canonicalizes_mime_and_preserves_content_identity() {
    let bytes = [0xff, 0xd8, 0x00, 0xff, 0xd9];
    let blob_value = annotated_blob_value(&bytes);
    let original_identity = blob_value.blob().unwrap().content_identity();
    let environment = BTreeMap::from([("asset".to_owned(), blob_value.clone())]);

    let annotated = evaluate_expression_ovb2(
        "sys.blob.annotate(asset, \"Application/JavaScript\", \"MJS\")",
        &environment,
        Limits::default(),
    )
    .expect("dispatch generated native Blob annotation");
    assert_eq!(annotated.format(), ValueFormat::Ovb2);
    let blob = annotated.blob().expect("annotated OVB-2 Blob");
    assert_eq!(blob.media_type(), "text/javascript");
    assert_eq!(blob.suffix(), Some("mjs"));
    assert_eq!(blob.content_identity(), original_identity);
    assert_eq!(blob.read_to_end().unwrap(), bytes);

    let preferred = evaluate_expression_ovb2(
        "sys.blob.annotate(asset, \"text/javascript\", \"JS\")",
        &environment,
        Limits::default(),
    )
    .expect("preferred suffix canonicalizes to no explicit hint");
    let preferred_blob = preferred.blob().unwrap();
    assert_eq!(preferred_blob.media_type(), "text/javascript");
    assert_eq!(preferred_blob.suffix(), None);
    assert_eq!(preferred_blob.content_identity(), original_identity);
}

#[test]
fn native_sys_blob_annotate_reports_mime_policy_failures() {
    let blob_value = annotated_blob_value(b"payload");
    let environment = BTreeMap::from([("asset".to_owned(), blob_value)]);
    for (call, expected) in [
        (
            "sys.blob.annotate(asset, \"image/*\")",
            "sys.blob.invalid_media_type",
        ),
        (
            "sys.blob.annotate(asset, \"image/jpeg\", \"../jpg\")",
            "sys.blob.invalid_suffix",
        ),
        (
            "sys.blob.annotate(asset, \"image/jpeg\", \"png\")",
            "sys.blob.incompatible_suffix",
        ),
    ] {
        let error = evaluate_expression_ovb2(call, &environment, Limits::default())
            .expect_err("invalid MIME-1 annotation must fail");
        assert_eq!(error.code(), expected, "{call}");
    }
}

struct AnnotatedBlobEcho {
    received: Option<ContextValue>,
}

impl Ovb2EffectHandler for AnnotatedBlobEcho {
    fn handle(
        &mut self,
        _callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let Some(value) = arguments.first() else {
            return Ok(None);
        };
        self.received = Some(value.clone());
        Ok(Some(value.clone()))
    }
}

#[test]
fn ovb2_native_effect_binding_round_trips_annotated_blob_without_conversion() {
    let bytes = [0xff, 0xd8, 0x00, 0xff, 0xd9];
    let blob_value = annotated_blob_value(&bytes);
    let environment = BTreeMap::from([("asset".to_owned(), blob_value.clone())]);
    let mut effects = AnnotatedBlobEcho { received: None };

    let returned = evaluate_expression_ovb2_with_effects(
        "host.echo(asset)",
        &environment,
        Limits::default(),
        &mut effects,
    )
    .expect("dispatch OVB-2 annotated Blob through native binding");

    let received = effects.received.expect("native binding received argument");
    assert_eq!(received.format(), ValueFormat::Ovb2);
    assert!(matches!(received.raw(), Raw::Tag(OVB2_BLOB_TAG, _)));
    assert!(
        CanonicalValue::new(received.raw().clone()).is_err(),
        "annotated Blob must not be flattened through the OVB-1 canonical ABI"
    );
    assert_eq!(received, blob_value);
    assert_eq!(returned, blob_value);

    let returned_blob = returned.blob().expect("returned annotated Blob");
    assert_eq!(returned_blob.media_type(), "image/jpeg");
    assert_eq!(returned_blob.suffix(), Some("jpeg"));
    assert_eq!(returned_blob.read_to_end().expect("read Blob bytes"), bytes);
}

struct LegacyBytesEcho {
    received: Option<CanonicalValue>,
}

impl EffectHandler for LegacyBytesEcho {
    fn handle(
        &mut self,
        _callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Some(value) = arguments.first() else {
            return Ok(None);
        };
        self.received = Some(value.clone());
        Ok(Some(value.clone()))
    }
}

#[test]
fn legacy_effect_handler_still_receives_and_returns_canonical_ovb1_bytes() {
    let bytes = vec![0xff, 0xd8, 0x00, 0xff, 0xd9];
    let value = CanonicalValue::new(Raw::Bytes(bytes.clone())).expect("canonical OVB-1 bytes");
    let environment: Environment = BTreeMap::from([("asset".to_owned(), value.clone())]);
    let mut effects = LegacyBytesEcho { received: None };

    let returned = evaluate_expression_with_effects(
        "host.echo(asset)",
        &environment,
        Limits::default(),
        &mut effects,
    )
    .expect("dispatch through legacy EffectHandler ABI");

    assert_eq!(effects.received, Some(value.clone()));
    assert_eq!(returned, value);
}

#[test]
fn ovb2_finite_relation_is_rejected_before_ovb1_validation() {
    let row = annotated_blob_value(b"relation row");
    let relation = ContextValue::new(
        ValueFormat::Ovb2,
        Raw::Tag(
            60027,
            Box::new(Raw::Array(vec![
                Raw::Bool(true),
                Raw::Array(vec![row.raw().clone()]),
            ])),
        ),
    )
    .expect("valid OVB-2 finite Relation containing an annotated Blob row");
    assert!(
        CanonicalValue::new(relation.raw().clone()).is_err(),
        "the OVB-1 validator must reject this OVB-2-only Relation payload"
    );
    let environment = BTreeMap::from([("relation".to_owned(), relation)]);

    let failure = evaluate_expression_ovb2("relation", &environment, Limits::default())
        .expect_err("finite Relation is outside the evaluator value subset");
    assert_eq!(failure.code(), "ORNA-EVAL-UNSUPPORTED");
}
