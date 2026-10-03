use std::collections::BTreeMap;

use orna_evaluator_v1::{
    AdmittedReplSession, Environment, Functions, Limits, NominalDefinition, NominalDefinitions,
    NominalField, PureFunction, evaluate_expression, evaluate_expression_with_functions,
    evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn int_value(value: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Int(value.into())).unwrap()
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_evaluator_v1::reference_standard_sources() {
        let parsed = orna_syntax_v1::parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }
    functions
}

fn eval_pinned(source: &str) -> Result<CanonicalValue, orna_evaluator_v1::EvaluationError> {
    eval_pinned_with_environment(source, Environment::new())
}

fn eval_pinned_with_environment(
    source: &str,
    environment: Environment,
) -> Result<CanonicalValue, orna_evaluator_v1::EvaluationError> {
    evaluate_expression_with_functions(source, &environment, &pinned_functions(), Limits::default())
}

fn eval_pinned_with_nominals(
    source: &str,
    definitions: &NominalDefinitions,
) -> Result<CanonicalValue, orna_evaluator_v1::EvaluationError> {
    let parsed = orna_syntax_v1::parse_expression(source);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    evaluate_with_functions_and_nominals(
        &parsed.value,
        &Environment::new(),
        &pinned_functions(),
        definitions,
        Limits::default(),
    )
}

fn money_value(coefficient: i64, exponent10: i64, currency: [u8; 16]) -> CanonicalValue {
    CanonicalValue::new(Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(coefficient.into()),
                    Raw::Int(exponent10.into()),
                ])),
            ),
            Raw::Tag(37, Box::new(Raw::Bytes(currency.to_vec()))),
        ])),
    ))
    .unwrap()
}

#[test]
fn pinned_encoding_modules_compute_canonical_values_and_reject_noncanonical_input() {
    let _ = pinned_functions();
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-encoding-6u13r.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-call-encoding-base64-6u13r.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bytes(b"a".to_vec())).unwrap()))
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-codec-json-exact-rl767.orna")),
        Ok(Some(int_value(9_007_199_254_740_993)))
    );
    let expected_text = text_value("GCo=");
    assert_eq!(
        eval_pinned(include_str!(
            "fixtures/stdlib-codec-ovb-transport-rl767.orna"
        )),
        Ok(expected_text)
    );
    assert_eq!(
        eval_pinned(include_str!(
            "fixtures/stdlib-codec-ovb-roundtrip-rl767.orna"
        )),
        Ok(int_value(42))
    );
    assert_eq!(
        eval_pinned(include_str!("fixtures/stdlib-codec-orna-encode-rl767.orna")),
        Ok(text_value("42\n"))
    );
    assert_eq!(
        eval_pinned("std.encoding.orna.decode(\"42\\n\", as: Int)"),
        Ok(int_value(42))
    );
    assert_eq!(
        eval_pinned(include_str!("fixtures/stdlib-codec-json-exact-rl767.orna")),
        Ok(int_value(9_007_199_254_740_993))
    );
    assert_eq!(
        eval_pinned(include_str!("fixtures/stdlib-codec-json-record-rl767.orna")),
        Ok(text_value("{\"answer\":42,\"label\":\"exact\"}"))
    );
    for fixture in [
        include_str!("fixtures/stdlib-codec-json-stream-rl767.orna"),
        include_str!("fixtures/stdlib-codec-orna-stream-rl767.orna"),
    ] {
        assert_eq!(
            eval_pinned(fixture).unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED",
            "stream resources cannot be encoded as interchange data: {fixture}"
        );
    }
    for (fixture, source) in [
        (
            include_str!("fixtures/stdlib-codec-base64-invalid-rl767.orna"),
            "std.encoding.base64.decode(\"Zh==\")",
        ),
        (
            include_str!("fixtures/stdlib-codec-json-duplicate-rl767.orna"),
            "std.encoding.json.decode(\"\\u{7b}\\\"x\\\":1,\\\"x\\\":2\\u{7d}\")",
        ),
        (
            include_str!("fixtures/stdlib-codec-ovb-noncanonical-rl767.orna"),
            "std.encoding.ovb.decode(std.encoding.base64.decode(\"GAE=\"), as: Int)",
        ),
    ] {
        assert_eq!(
            eval_pinned(fixture).unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "{source}"
        );
    }
    assert_eq!(
        eval_pinned(include_str!(
            "fixtures/stdlib-codec-json-witness-mismatch-rl767.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-EVAL-VALUE"
    );
    let payload = Environment::from([(
        "payload".into(),
        CanonicalValue::new(Raw::Bytes(b"hello".to_vec())).unwrap(),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-base64-encode-6u13r.orna"),
            &payload,
            Limits::default(),
        ),
        Ok(text_value("aGVsbG8="))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-money-codecs-rl767.orna");
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

#[test]
fn standard_base64_roundtrips_each_padding_shape_and_rejects_noncanonical_tails() {
    let encode = include_str!("fixtures/stdlib-base64-edge-encode-etsj1.orna");
    let decode = include_str!("fixtures/stdlib-base64-edge-decode-etsj1.orna");
    for (bytes, encoded) in [
        (&[][..], ""),
        (&[0][..], "AA=="),
        (&[0, 1][..], "AAE="),
        (&[0, 1, 2][..], "AAEC"),
        (&[255][..], "/w=="),
        (&[255, 238][..], "/+4="),
    ] {
        let environment = Environment::from([(
            "payload".into(),
            CanonicalValue::new(Raw::Bytes(bytes.to_vec())).unwrap(),
        )]);
        assert_eq!(
            eval_pinned_with_environment(encode, environment),
            Ok(text_value(encoded)),
            "Base64 encoding of {bytes:?}"
        );

        let environment = Environment::from([("input".into(), text_value(encoded))]);
        assert_eq!(
            eval_pinned_with_environment(decode, environment),
            Ok(CanonicalValue::new(Raw::Bytes(bytes.to_vec())).unwrap()),
            "Base64 decoding of {encoded:?}"
        );
    }

    for invalid in [
        "A===",   // padding in a data position
        "AAA",    // omitted required padding
        "-w==",   // URL-safe alphabet is a separate profile
        "AA==\n", // whitespace is not ignored
        "AA=A",   // padding before the final position
        "AB==",   // nonzero unused four trailing bits
        "AAB=",   // nonzero unused two trailing bits
    ] {
        let environment = Environment::from([("input".into(), text_value(invalid))]);
        assert_eq!(
            eval_pinned_with_environment(decode, environment)
                .unwrap_err()
                .code(),
            "ORNA-EVAL-VALUE",
            "Base64 input {invalid:?} must be rejected"
        );
    }
}

#[test]
fn exact_money_allocation_distributes_remainders_stably_and_preserves_total() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-money-rl767.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-money-behavior-rl767.orna")),
        Ok(Some(bool_value(true)))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-money-codecs-rl767.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    assert!(without_std.submit("use std.money;").is_err());
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));

    let currency = [0x5a; 16];
    let amount = money_value(1234, -2, currency);
    let mut environment = Environment::new();
    environment.insert("amount".into(), amount);
    assert_eq!(
        eval_pinned_with_environment(
            include_str!("fixtures/stdlib-money-currency-rl767.orna"),
            environment,
        ),
        Ok(CanonicalValue::new(Raw::Array(vec![
            money_value(309, -2, currency).raw().clone(),
            money_value(309, -2, currency).raw().clone(),
            money_value(308, -2, currency).raw().clone(),
            money_value(308, -2, currency).raw().clone(),
        ]))
        .unwrap())
    );
    assert_eq!(
        eval_pinned(include_str!("fixtures/stdlib-money-format-values-rl767.orna")),
        Ok(bool_value(true))
    );
    assert_eq!(
        eval_pinned(include_str!("fixtures/stdlib-money-format-unsupported-locale-rl767.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn json_schema_decode_preserves_null_defaults_and_unknown_field_policy() {
    let default = orna_syntax_v1::parse_expression("\"fallback\"");
    assert!(default.is_ok(), "{:#?}", default.diagnostics);
    let definition = NominalDefinition::new(
        [0x11; 16],
        None,
        vec![
            NominalField::public([0x22; 16], "answer"),
            NominalField::public_with_default([0x33; 16], "label", default.value),
        ],
    );
    let definitions = NominalDefinitions::from([("CodecRecord".into(), definition)]);

    for fixture in [
        include_str!("fixtures/stdlib-codec-json-nominal-ignore-extra-rl767.orna"),
        include_str!("fixtures/stdlib-codec-json-nominal-default-rl767.orna"),
        include_str!("fixtures/stdlib-codec-json-nominal-explicit-null-rl767.orna"),
    ] {
        let actual = eval_pinned_with_nominals(fixture, &definitions)
            .unwrap_or_else(|error| panic!("schema decode failed: {}", error.code()));
        assert_eq!(
            actual,
            bool_value(true),
            "{fixture}"
        );
    }
    for fixture in [
        include_str!("fixtures/stdlib-codec-json-nominal-unknown-rejected-rl767.orna"),
        include_str!("fixtures/stdlib-codec-json-nominal-required-missing-rl767.orna"),
    ] {
        assert_eq!(
            eval_pinned_with_nominals(fixture, &definitions)
                .unwrap_err()
                .code(),
            "ORNA-EVAL-VALUE",
            "{fixture}"
        );
    }
}
