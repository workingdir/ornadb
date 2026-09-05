use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, EvaluationError, Limits, evaluate_expression, evaluate_function, evaluate_parsed,
    evaluate_repl,
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

fn invoke(source: &str, arguments: &Environment, limits: Limits) -> Result<Value, EvaluationError> {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let orna_syntax_v1::Declaration::Function { signature, body } =
        &parsed.value.items[0].declaration
    else {
        panic!("function expected");
    };
    evaluate_function(
        &signature.parameters,
        body,
        &Environment::new(),
        arguments,
        limits,
    )
}

fn functions_from_source(source: &str) -> orna_evaluator_v1::Functions {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration else {
                panic!("function expected")
            };
            (
                signature.name,
                orna_evaluator_v1::PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

fn call_module(source: &str, expression: &str, limits: Limits) -> Result<Value, EvaluationError> {
    let functions = functions_from_source(source);
    let parsed = orna_syntax_v1::parse_expression(expression);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    orna_evaluator_v1::evaluate_with_functions(
        &parsed.value,
        &Environment::new(),
        &functions,
        limits,
    )
}

#[test]
fn host_invocation_uses_the_same_named_function_namespace() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1; fn entry(value = helper(40)) = helper(value);",
    );
    assert_eq!(
        orna_evaluator_v1::invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default()
        )
        .unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        code(orna_evaluator_v1::invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 4,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(orna_evaluator_v1::invoke_named(
            "missing",
            &functions,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn source_calls_bind_positional_named_and_nested_defaults() {
    let source = "fn twice(value: Int) = value + value; fn add(value: Int, extra = twice(3)) = value + extra;";
    for expression in [
        "add(10)",
        "add(value: 10)",
        "add(10, extra: 6)",
        "add(extra: 6, value: 10)",
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(16.into())
        );
    }
    assert_eq!(
        call_module(source, "add(twice(5))", Limits::default()).unwrap(),
        Value::int(16.into())
    );
    for expression in [
        "add()",
        "add(1, 2, 3)",
        "add(1, value: 2)",
        "add(value: 1, 2)",
        "add(unknown: 1)",
    ] {
        assert_eq!(
            code(call_module(source, expression, Limits::default())),
            "ORNA-EVAL-ARGUMENT",
            "{expression}"
        );
    }
}

#[test]
fn source_calls_are_lexical_and_respect_value_shadowing() {
    let source = "fn inner() = secret; fn outer(secret: Int) = inner();";
    assert_eq!(
        code(call_module(source, "outer(7)", Limits::default())),
        "ORNA-EVAL-NAME"
    );
    let source = "fn identity(value: Int) = value;";
    assert_eq!(
        code(call_module(
            source,
            "if true { let identity = 1; identity(2) } else { 0 }",
            Limits::default()
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn source_call_arguments_evaluate_in_source_order() {
    let source = "fn encode(a: Int, b: Int) = 10 * a + b; fn caller() { let counter = 0; encode(b: if true { counter += 1; counter } else { 0 }, a: if true { counter += 1; counter } else { 0 }) }";
    assert_eq!(
        call_module(source, "caller()", Limits::default()).unwrap(),
        Value::int(21.into())
    );
}

#[test]
fn closures_capture_immutable_snapshots_and_support_nested_calls() {
    for (source, expression, expected) in [
        (
            "fn make(seed: Int) = value => seed + value;",
            "make(10)(5)",
            15,
        ),
        (
            "fn run() { let seed = 1; let read = () => seed; seed = 9; read() }",
            "run()",
            1,
        ),
        (
            "fn run() { let seed = 1; let replace = seed => seed + 1; replace(10) }",
            "run()",
            11,
        ),
        ("fn make(a: Int) = b => c => a + b + c;", "make(1)(2)(3)", 6),
        (
            "fn run() { let seed = 1; let local = () => { let seed = 10; seed += 1; seed }; local() }",
            "run()",
            11,
        ),
        (
            "fn run() { let seed = 1; let local = seed => { seed += 1; seed }; local(10) }",
            "run()",
            11,
        ),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(expected.into())
        );
    }
    let source = "fn run() { let seed = 1; let mutate = () => { seed += 1; seed }; mutate() }";
    assert_eq!(
        code(call_module(source, "run()", Limits::default())),
        "ORNA-EVAL-IMMUTABLE-CAPTURE"
    );
}

#[test]
fn function_values_pass_through_locals_arguments_and_collections() {
    let source = "fn increment(value: Int) = value + 1; fn apply(operation, value: Int) = operation(value); fn run() { let choices = [increment, value => value * 2]; apply(choices[0], 20) + apply(choices[1], 10) }";
    assert_eq!(
        call_module(source, "run()", Limits::default()).unwrap(),
        Value::int(41.into())
    );
    assert_eq!(evaluate("(value => value)(2)"), Value::int(2.into()));
    assert_eq!(evaluate("((a, b) => a + b)(1, 2)"), Value::int(3.into()));
}

#[test]
fn anonymous_pipeline_stages_share_callable_binding_and_limits() {
    assert_eq!(evaluate("10 | (value => value + 2)"), Value::int(12.into()));
    assert_eq!(
        evaluate("(10 | (value => value + 2)) | (value => value * 2)"),
        Value::int(24.into())
    );
    assert_eq!(
        code(evaluate_expression(
            "((value => value)(1))",
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
    for expression in ["(a => a)(1, 2)", "(() => 1)(2)"] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-ARGUMENT"
        );
    }
    for expression in [
        "value => value",
        "[value => value]",
        "{ callback: value => value }",
        "(value => value) == (value => value)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
}

#[test]
fn pipelines_insert_the_input_before_explicit_arguments_and_defaults() {
    let source =
        "fn add(value: Int, extra = 6) = value + extra; fn double(value: Int) = value * 2;";
    for expression in [
        "10 | add",
        "10 | add()",
        "10 | add(extra: 6)",
        "10 | add(6)",
        "5 | double | add",
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(16.into()),
            "{expression}"
        );
    }
    for expression in [
        "10 | add(value: 1)",
        "10 | add(extra: 1, extra: 2)",
        "10 | add(1, 2)",
    ] {
        assert_eq!(
            code(call_module(source, expression, Limits::default())),
            "ORNA-EVAL-ARGUMENT",
            "{expression}"
        );
    }
    assert_eq!(
        code(call_module(
            "fn no_input() = 1;",
            "10 | no_input",
            Limits::default()
        )),
        "ORNA-EVAL-ARGUMENT"
    );
}

#[test]
fn pipeline_input_runs_once_and_before_stage_arguments() {
    let source = "fn encode(a: Int, b: Int) = 10 * a + b; fn caller() { let counter = 0; (if true { counter += 1; counter } else { 0 }) | encode(b: if true { counter += 1; counter } else { 0 }) }";
    assert_eq!(
        call_module(source, "caller()", Limits::default()).unwrap(),
        Value::int(12.into())
    );
    assert_eq!(
        code(call_module(
            "fn add(a: Int, b: Int) = a + b;",
            "(1 / 0) | add(missing)",
            Limits::default()
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(call_module(
            "fn recurse(n: Int) = n | recurse;",
            "1 | recurse",
            Limits {
                max_steps: 12,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn math_pipelines_and_mixed_named_calls_share_argument_positions() {
    for (expression, expected) in [
        ("41 | std.math.increment", 42),
        ("41 | std.math.increment()", 42),
        ("3 | std.math.max(right: 7)", 7),
        ("std.math.max(3, right: 7)", 7),
        ("10 | std.math.clamp(min: 1, max: 5)", 5),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    for expression in [
        "3 | std.math.max(left: 7)",
        "std.math.max(left: 3, 7)",
        "std.math.max(left: 3, left: 7)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
}

#[test]
fn recursive_calls_terminate_or_hit_shared_limits() {
    let source = "fn factorial(n: Int) = if n == 0 { 1 } else { n * factorial(n - 1) };";
    assert_eq!(
        call_module(source, "factorial(5)", Limits::default()).unwrap(),
        Value::int(120.into())
    );
    for limits in [
        Limits {
            max_steps: 8,
            ..Limits::default()
        },
        Limits {
            max_depth: 8,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            code(call_module("fn recur() = recur();", "recur()", limits)),
            "ORNA-EVAL-LIMIT"
        );
    }
    let source = "fn small() = 1 + 2; fn combined() = small() + small();";
    assert_eq!(
        code(call_module(
            source,
            "combined()",
            Limits {
                max_steps: 6,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn function_defaults_return_values_and_see_earlier_parameters() {
    let source =
        "fn compute(first: Int, second = first + 1, third = second + 1) = first + second + third;";
    let arguments = Environment::from([("first".into(), Value::int(10.into()))]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(33.into())
    );
    let arguments = Environment::from([
        ("first".into(), Value::int(10.into())),
        ("second".into(), Value::int(20.into())),
    ]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(51.into())
    );
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(51.into())
    );
}

#[test]
fn supplied_arguments_do_not_evaluate_their_defaults() {
    let source = "fn choose(value: Int = 1 / 0) = value;";
    let arguments = Environment::from([("value".into(), Value::int(7.into()))]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(7.into())
    );
    assert_eq!(
        code(invoke(source, &Environment::new(), Limits::default())),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn function_defaults_and_body_share_a_single_step_budget() {
    let source = "fn compute(first = 1 + 2, second = 3 + 4) = first + second;";
    let limits = Limits {
        max_steps: 6,
        ..Limits::default()
    };
    assert_eq!(
        code(invoke(source, &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_steps: 9,
        ..Limits::default()
    };
    assert_eq!(
        invoke(source, &Environment::new(), limits).unwrap(),
        Value::int(10.into())
    );
}

#[test]
fn function_argument_admission_precedes_default_evaluation_and_redacts_errors() {
    for (source, arguments) in [
        (
            "fn compute(first = 1 / 0, second: Int) = first;",
            Environment::new(),
        ),
        (
            "fn compute(first = 1 / 0) = first;",
            Environment::from([("secret".into(), Value::int(1.into()))]),
        ),
        (
            "fn compute(first: Int, first: Int) = first;",
            Environment::from([("first".into(), Value::int(1.into()))]),
        ),
    ] {
        let error = invoke(source, &arguments, Limits::default()).unwrap_err();
        assert_eq!(error.code(), "ORNA-EVAL-ARGUMENT");
        assert_eq!(error.diagnostic().message(), "<redacted>");
    }
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
fn coalesces_present_and_missing_optional_values_without_evaluating_dead_rhs() {
    let mut environment = Environment::from([(
        "present".into(),
        Value::option(Some(Value::int(7.into()))).unwrap(),
    )]);
    assert_eq!(
        evaluate_expression("present ?? missing", &environment, Limits::default()).unwrap(),
        Value::int(7.into())
    );

    environment.insert("missing".into(), Value::option(None).unwrap());
    assert_eq!(
        evaluate_expression("missing ?? 9", &environment, Limits::default()).unwrap(),
        Value::int(9.into())
    );
    assert_eq!(evaluate("null ?? 11"), Value::int(11.into()));
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

#[test]
fn evaluates_local_assignments_and_finite_list_for_mutations() {
    assert_eq!(
        evaluate("if true { let total = 0; for value in [1, 2, 3] { total += value; }; total }"),
        Value::int(6.into())
    );
    assert_eq!(
        evaluate("if true { let total = 0; if true { total = 4; }; total }"),
        Value::int(4.into())
    );
    assert_eq!(evaluate("if true { assert true; 5 }"), Value::int(5.into()));
    assert_eq!(
        code(evaluate_expression(
            "if true { assert false; 5 }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-ASSERT"
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { let total = 0; for value in 1 { total += value; }; total }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
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
        ("(value => value)", "ORNA-EVAL-UNSUPPORTED"),
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
