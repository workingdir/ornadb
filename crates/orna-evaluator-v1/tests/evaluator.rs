use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, EvaluationError, Limits, evaluate_expression, evaluate_parsed, evaluate_repl,
};
use orna_syntax_v1::{Expr, Pattern, RecordField, Statement, SyntaxSpan};
use orna_value_v1::{Raw, Value};

fn evaluate(source: &str) -> Value {
    evaluate_expression(source, &Environment::new(), Limits::default())
        .unwrap_or_else(|error| panic!("{}: {}", source, error.code()))
}
fn code(result: Result<Value, EvaluationError>) -> String {
    result.unwrap_err().code().to_owned()
}

#[test]
fn evaluates_literals_collections_bindings_and_math() {
    let value = evaluate(
        "if true { let point = (std.math.increment(1), 2.5, 3.0f); { label: \"ok\", values: [point, null, true] } }",
    );
    assert_eq!(
        value.raw(),
        &Raw::Map(vec![
            (Raw::Text("label".into()), Raw::Text("ok".into())),
            (
                Raw::Text("values".into()),
                Raw::Array(vec![
                    Raw::Array(vec![
                        Raw::Int(2.into()),
                        Raw::Tag(
                            60000,
                            Box::new(Raw::Array(vec![Raw::Int(25.into()), Raw::Int((-1).into())]))
                        ),
                        Raw::Float(3.0f64.to_bits())
                    ]),
                    Raw::Null,
                    Raw::Bool(true),
                ])
            ),
        ])
    );
}

#[test]
fn uses_environment_and_short_circuiting_deterministically() {
    let mut environment = BTreeMap::new();
    environment.insert("count".into(), Value::int(41.into()));
    assert_eq!(
        evaluate_expression("count + 1", &environment, Limits::default()).unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        evaluate("false && missing"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("true || missing"),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn supports_comparison_boolean_and_allowlisted_math() {
    assert_eq!(
        evaluate(
            "std.math.clamp(9, 0, 5) == std.math.max(3, 5) && std.math.is_zero(std.math.decrement(1))"
        ),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1.25 + 0.75"),
        Value::decimal(2.into(), 0.into()).unwrap()
    );
    assert_eq!(
        evaluate("{ alphabet: 1, z: 2 }").raw(),
        &Raw::Map(vec![
            (Raw::Text("z".into()), Raw::Int(2.into())),
            (Raw::Text("alphabet".into()), Raw::Int(1.into())),
        ])
    );
    assert_eq!(
        evaluate_repl("std.math.min(7, 3)", &Environment::new(), Limits::default()).unwrap(),
        Value::int(3.into())
    );
}

#[test]
fn evaluates_selection_indexing_named_calls_and_case_patterns() {
    assert_eq!(
        evaluate(
            "{ point: { x: 7 }, values: [3, 5] }.point.x + { point: { x: 7 }, values: [3, 5] }.values[1]"
        ),
        Value::int(12.into())
    );
    assert_eq!(
        evaluate("std.math.clamp(max: 5, value: 9, min: 0)"),
        Value::int(5.into())
    );
    assert_eq!(
        evaluate("case [2, 3] { [left, right] if left < right: right, _: 0 }"),
        Value::int(3.into())
    );
    let span = SyntaxSpan::new(0, 0);
    let record_pattern = Pattern::Record {
        fields: vec![(
            "right".into(),
            Some(Pattern::Name("selected".into(), span.clone())),
            span.clone(),
        )],
        span: span.clone(),
    };
    let record_value = Expr::Record {
        fields: vec![RecordField {
            name: "right".into(),
            value: Expr::Literal {
                text: "9".into(),
                kind: orna_syntax_v1::LiteralKind::Integer,
                span: span.clone(),
            },
            span: span.clone(),
        }],
        span: span.clone(),
    };
    let expression = Expr::Block {
        statements: vec![Statement::Let {
            pattern: record_pattern,
            annotation: None,
            value: record_value,
            span: span.clone(),
        }],
        tail: Some(Box::new(Expr::Name {
            text: "selected".into(),
            span: span.clone(),
        })),
        span,
    };
    assert_eq!(
        evaluate_parsed(&expression, &Environment::new(), Limits::default()).unwrap(),
        Value::int(9.into())
    );
}

fn object_id(byte: u8) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(vec![byte; 16])))
}

fn enum_value(type_id: u8, variant_id: u8, payload: Option<Raw>) -> Value {
    Value::new(Raw::Tag(
        60008,
        Box::new(Raw::Array(vec![
            object_id(type_id),
            object_id(variant_id),
            payload.unwrap_or(Raw::Null),
        ])),
    ))
    .unwrap()
}

fn record_payload(fields: Vec<(&str, Raw)>) -> Raw {
    Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![
            Raw::Null,
            Raw::Array(
                fields
                    .into_iter()
                    .map(|(name, value)| Raw::Array(vec![Raw::Text(name.into()), value]))
                    .collect(),
            ),
        ])),
    )
}

#[test]
fn matches_enum_labels_payload_fields_and_interpolates_bound_strings() {
    let ready = enum_value(1, 2, None);
    let waiting_label = enum_value(1, 3, None);
    let waiting = enum_value(
        1,
        3,
        Some(record_payload(vec![(
            "reason",
            Raw::Text("maintenance".into()),
        )])),
    );
    let mut environment = Environment::from([
        ("Availability.ready".into(), ready.clone()),
        ("Availability.waiting".into(), waiting_label),
        ("value".into(), waiting),
    ]);
    assert_eq!(
        evaluate_expression(
            "case value { Availability.ready: \"ready\", Availability.waiting { reason }: \"waiting: {reason}\" }",
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("waiting: maintenance".into())).unwrap()
    );

    environment.insert("value".into(), ready);
    assert_eq!(
        evaluate_expression(
            "case value { Availability.ready: \"ready\", Availability.waiting { reason }: reason }",
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("ready".into())).unwrap()
    );
}

#[test]
fn matches_tagged_optional_some_and_null() {
    let mut environment = Environment::from([(
        "value".into(),
        Value::option(Some(Value::new(Raw::Text("Kieran".into())).unwrap())).unwrap(),
    )]);
    let source = "case value { Some(name): name, null: \"anonymous\" }";
    assert_eq!(
        evaluate_expression(source, &environment, Limits::default()).unwrap(),
        Value::new(Raw::Text("Kieran".into())).unwrap()
    );

    environment.insert("value".into(), Value::option(None).unwrap());
    assert_eq!(
        evaluate_expression(source, &environment, Limits::default()).unwrap(),
        Value::new(Raw::Text("anonymous".into())).unwrap()
    );
}

#[test]
fn rejects_fail_closed_cases_with_redacted_stable_diagnostics() {
    for (source, expected) in [
        ("unknown", "ORNA-EVAL-NAME"),
        ("std.math.abs(1)", "ORNA-EVAL-UNSUPPORTED"),
        (
            "std.math.min(right: 2, left: 1, extra: 0)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("1 / 0", "ORNA-EVAL-DIVIDE-BY-ZERO"),
        ("{ value: 1 }.missing", "ORNA-EVAL-FIELD"),
        ("[1][2]", "ORNA-EVAL-INDEX"),
        ("[1][true]", "ORNA-EVAL-TYPE"),
        ("case 1 { 2: 2 }", "ORNA-EVAL-NO-MATCH"),
        ("case 1 { Unknown.value: 1, _: 0 }", "ORNA-EVAL-UNSUPPORTED"),
        ("case 1 { Other(inside): inside }", "ORNA-EVAL-UNSUPPORTED"),
        ("\"value: {1}\"", "ORNA-EVAL-TYPE"),
        ("(value => value)(2)", "ORNA-EVAL-UNSUPPORTED"),
        ("{ let x = 1; x = 2; x }", "ORNA-EVAL-PARSE"),
        ("fn x() = 1", "ORNA-EVAL-PARSE"),
        ("2026-09-05", "ORNA-EVAL-UNSUPPORTED"),
    ] {
        let failure =
            evaluate_expression(source, &Environment::new(), Limits::default()).unwrap_err();
        assert_eq!(failure.code(), expected, "{source}");
        assert_eq!(failure.diagnostic().message(), "<redacted>");
        assert!(!failure.diagnostic().message().contains(source));
    }
}

#[test]
fn rejects_resource_limits_before_work_can_expand() {
    let limits = Limits {
        max_steps: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("1 + 2", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_source_bytes: 3,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("1234", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("[1, 2]", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            "case [1, 2] { [a, b]: a }",
            &Environment::new(),
            limits
        )),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    let environment = Environment::from([(
        "value".into(),
        Value::option(Some(Value::new(Raw::Text("nested".into())).unwrap())).unwrap(),
    )]);
    assert_eq!(
        code(evaluate_expression(
            "case value { Some(Some(name)): name, _: \"none\" }",
            &environment,
            limits,
        )),
        "ORNA-EVAL-LIMIT"
    );
}
