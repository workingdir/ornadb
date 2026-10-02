use std::collections::BTreeMap;

use orna_evaluator_v1::{
    AdmittedReplSession, Environment, Functions, Limits, PureFunction,
    evaluate_expression, evaluate_expression_with_functions,
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
}
