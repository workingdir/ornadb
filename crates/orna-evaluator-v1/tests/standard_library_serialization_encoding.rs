use orna_evaluator_v1::{AdmittedReplSession, Environment, Limits, evaluate_expression};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_encoding_modules_are_optional_and_base64_executes_with_rfc4648_values() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-encoding-6u13r.orna")),
        Ok(None)
    );
    for codec_call in [
        include_str!("fixtures/stdlib-call-encoding-orna-6u13r.orna"),
        include_str!("fixtures/stdlib-call-encoding-ovb-6u13r.orna"),
        include_str!("fixtures/stdlib-call-encoding-json-6u13r.orna"),
    ] {
        assert_eq!(
            with_std.submit(codec_call).unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
    assert_eq!(
        with_std.submit(include_str!(
            "fixtures/stdlib-call-encoding-base64-6u13r.orna"
        )),
        Ok(Some(canonical(Raw::Bytes(b"a".to_vec()))))
    );
    let payload = Environment::from([("payload".into(), canonical(Raw::Bytes(b"hello".to_vec())))]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-base64-encode-6u13r.orna"),
            &payload,
            Limits::default(),
        ),
        Ok(canonical(Raw::Text("aGVsbG8=".into())))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-encoding-6u13r.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-encoding-6u13r.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
}
