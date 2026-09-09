use std::collections::BTreeMap;

use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Limits, evaluate_expression, evaluate_function,
    evaluate_parsed, evaluate_repl, invoke_named, invoke_named_with_effects,
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

#[derive(Default)]
struct NoteEffects {
    calls: Vec<Vec<Value>>,
}

impl EffectHandler for NoteEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text != "Note" || name != "insert" {
            return Ok(None);
        }
        self.calls.push(arguments.to_vec());
        Ok(Some(Value::int(arguments.len().into())))
    }
}

struct UnitEffects;

impl EffectHandler for UnitEffects {
    fn handle(&mut self, callee: &Expr, _: &[Value]) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text == "Note" && name == "delete" {
            Ok(Some(Value::unit()))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn effect_handler_can_return_unit_values() {
    let functions = functions_from_source("fn entry() = Note.delete(1);");
    let mut effects = UnitEffects;

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::unit()
    );
}

#[test]
fn admission_limits_reject_zero_configuration_and_count_source_bytes() {
    let limits = Limits {
        max_source_bytes: 2,
        max_collection_items: 2,
        ..Limits::default()
    };
    assert!(limits.check_source("é").is_ok());
    assert_eq!(
        limits.check_source("éa").unwrap_err().code(),
        "ORNA-EVAL-LIMIT"
    );
    assert!(limits.check_items(2).is_ok());
    assert_eq!(limits.check_items(3).unwrap_err().code(), "ORNA-EVAL-LIMIT");
    for index in 0..6 {
        let mut limits = Limits::default();
        match index {
            0 => limits.max_source_bytes = 0,
            1 => limits.max_steps = 0,
            2 => limits.max_depth = 0,
            3 => limits.max_collection_items = 0,
            4 => limits.max_string_bytes = 0,
            _ => limits.max_integer_digits = 0,
        }
        assert_eq!(
            limits.check_source("").unwrap_err().code(),
            "ORNA-EVAL-LIMIT"
        );
        assert_eq!(limits.check_items(0).unwrap_err().code(), "ORNA-EVAL-LIMIT");
    }
}

#[test]
fn source_namespace_entry_checks_values_and_limits_before_parsing() {
    let functions = functions_from_source("fn increment(value: Int) = value + 1;");
    let environment = Environment::from([("input".into(), Value::int(41.into()))]);
    assert_eq!(
        orna_evaluator_v1::evaluate_expression_with_functions(
            "input | increment",
            &environment,
            &functions,
            Limits::default()
        )
        .unwrap(),
        Value::int(42.into())
    );
    let failure = orna_evaluator_v1::evaluate_expression_with_functions(
        "invalid(",
        &environment,
        &functions,
        Limits {
            max_source_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(failure.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(failure.diagnostic().message(), "<redacted>");
    assert_eq!(
        code(orna_evaluator_v1::evaluate_expression_with_functions(
            "invalid(",
            &environment,
            &functions,
            Limits::default()
        )),
        "ORNA-EVAL-PARSE"
    );
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
fn effect_handler_runs_for_a_nested_non_pure_field_call_with_once_evaluated_arguments() {
    let functions = functions_from_source(
        "fn child(value: Int) = Note.insert(value); fn entry() { let counter = 0; child(if true { counter += 1; counter } else { 0 }); counter }",
    );
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::int(1.into())
    );
    assert_eq!(effects.calls, vec![vec![Value::int(1.into())]]);
}

#[test]
fn effect_handler_none_falls_through_without_intercepting_pure_calls() {
    let functions =
        functions_from_source("fn helper(value: Int) = value + 1; fn entry() = helper(41);");
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::int(42.into())
    );
    assert!(effects.calls.is_empty());
}

#[test]
fn invoke_named_remains_effect_free_for_non_pure_field_calls() {
    let functions = functions_from_source("fn entry() = Note.insert(1);");

    assert_eq!(
        code(invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
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
fn wildcard_and_structured_lambda_parameters_bind_by_position() {
    for (expression, expected) in [
        ("(_ => 7)(123)", 7),
        ("((_, _) => 7)(1, 2)", 7),
        ("10 | (_ => 7)", 7),
        ("(((a, b)) => a + b)((1, 2))", 3),
        ("(([a, b]) => a + b)([1, 2])", 3),
        ("(({a, b}) => a + b)({a: 1, b: 2})", 3),
        ("(1, 2) | (((a, b)) => a + b)", 3),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "(_ => 7)(1 / 0)",
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    for expression in ["(((a, b)) => a + b)(1)", "(([a, b]) => a + b)([1])"] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE"
        );
    }
    for expression in [
        "((_, _) => 7)(1)",
        "(((a, b)) => a + b)(a: 1, b: 2)",
        "(({a}, a) => a)({a: 1}, 2)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-ARGUMENT"
        );
    }
}

#[test]
fn named_functions_share_structured_parameter_binding_and_defaults() {
    let source = "fn add((a, b) = (1, 2)) = a + b; fn ignore(_, _) = 7;";
    for (expression, expected) in [
        ("add()", 3),
        ("add((10, 20))", 30),
        ("(10, 20) | add", 30),
        ("ignore(1, 2)", 7),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(expected.into())
        );
    }
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
fn std_bits_preserves_unbounded_signed_twos_complement_semantics() {
    for (expression, expected) in [
        ("std.bits.bit_or(0b1010, 0b0110)", 14),
        ("std.bits.bit_and(0b1010, 0b0110)", 2),
        ("std.bits.bit_xor(0b1010, 0b0110)", 12),
        ("std.bits.bit_not(0)", -1),
        ("std.bits.bit_not(-1)", 0),
        ("1 | std.bits.shift_left(80)", 1_i128 << 80),
        ("-5 | std.bits.shift_right(1)", -3),
        ("1 | std.bits.shift_right(100000000000000000000)", 0),
        ("-1 | std.bits.shift_right(100000000000000000000)", -1),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    for (expression, expected) in [
        ("std.bits.shift_left(1, -1)", "ORNA-EVAL-VALUE"),
        ("std.bits.shift_right(1, -1)", "ORNA-EVAL-VALUE"),
        ("std.bits.bit_or(1, true)", "ORNA-EVAL-TYPE"),
        ("std.bits.bit_not(1, 2)", "ORNA-EVAL-UNSUPPORTED"),
        ("std.bits.shift_left(1, 999999)", "ORNA-EVAL-LIMIT"),
        (
            "std.bits.bit_or(left: 1, right: 2)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_text_fallback_preserves_unicode_scalar_and_field_semantics() {
    let fields = Value::new(Raw::Array(vec![
        Raw::Text("alpha".into()),
        Raw::Text("".into()),
        Raw::Text("β".into()),
        Raw::Text("".into()),
    ]))
    .unwrap();
    for (expression, expected) in [
        (
            "std.text.trim(\"  café  \")",
            Value::new(Raw::Text("café".into())).unwrap(),
        ),
        ("std.text.split(\"alpha,,β,\", \",\")", fields),
        (
            "std.text.split(\"aβ\", \"\")",
            Value::new(Raw::Array(vec![
                Raw::Text("a".into()),
                Raw::Text("β".into()),
            ]))
            .unwrap(),
        ),
        (
            "std.text.join([\"a\", \"β\", \"\"], \":\")",
            Value::new(Raw::Text("a:β:".into())).unwrap(),
        ),
        (
            "std.text.starts_with(\"βeta\", \"βe\")",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "std.text.ends_with(\"βeta\", \"ta\")",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "std.text.contains(\"aβa\", \"β\")",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "std.text.replace(\"aaaa\", \"aa\", \"b\")",
            Value::new(Raw::Text("bb".into())).unwrap(),
        ),
        (
            "std.text.normalise(\"Cafe\u{301}\", \"NFC\")",
            Value::new(Raw::Text("Café".into())).unwrap(),
        ),
        (
            "std.text.normalise(\"Café\", \"NFD\")",
            Value::new(Raw::Text("Cafe\u{301}".into())).unwrap(),
        ),
        (
            "std.text.lower(\"İΣ\")",
            Value::new(Raw::Text("i̇ς".into())).unwrap(),
        ),
        (
            "std.text.upper(\"ß\")",
            Value::new(Raw::Text("SS".into())).unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_text_fallback_rejects_invalid_arguments_and_enforces_existing_limits() {
    for (expression, expected) in [
        ("std.text.trim(1)", "ORNA-EVAL-TYPE"),
        ("std.text.trim([\"a\"])", "ORNA-EVAL-TYPE"),
        ("std.text.join([\"a\", 1], \",\")", "ORNA-EVAL-TYPE"),
        ("std.text.join([\"a\"], [\",\"])", "ORNA-EVAL-TYPE"),
        ("std.text.replace(\"a\", \"a\")", "ORNA-EVAL-UNSUPPORTED"),
        (
            "std.text.split(value: \"a\", separator: \",\", extra: \"x\")",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.text.normalise(\"a\")", "ORNA-EVAL-UNSUPPORTED"),
        ("std.text.normalise(1, \"NFC\")", "ORNA-EVAL-TYPE"),
        ("std.text.normalise(\"a\", \"NFKC\")", "ORNA-EVAL-VALUE"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "std.text.replace(\"aaaa\", \"a\", \"bb\")",
            &Environment::new(),
            Limits {
                max_string_bytes: 7,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.text.split(\"abc\", \"\")",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_fallback_preserves_finite_list_ordering() {
    for (expression, expected) in [
        (
            "std.collection.chunk([1, 2, 3, 4, 5], 2)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(4.into())]),
                Raw::Array(vec![Raw::Int(5.into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.chunk([], 2)",
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            "std.collection.flatten([[1, 2], [], [3]])",
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.unique([2, 1, 2, [3], [3], 1, []])",
            Value::new(Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into())]),
                Raw::Array(vec![]),
            ]))
            .unwrap(),
        ),
        (
            "[2, 1, 2, 1] | std.collection.unique()",
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(1.into())])).unwrap(),
        ),
        (
            "std.collection.unique([1.0, 1.00, 2.0])",
            Value::new(Raw::Array(vec![
                Value::decimal(1.into(), 0.into()).unwrap().raw().clone(),
                Value::decimal(2.into(), 0.into()).unwrap().raw().clone(),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.distinct([2, 1, 2, [3], [3], 1, []])",
            Value::new(Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into())]),
                Raw::Array(vec![]),
            ]))
            .unwrap(),
        ),
        (
            "[2, 1, 2, 1] | std.collection.distinct()",
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(1.into())])).unwrap(),
        ),
        (
            "std.collection.distinct(values: [1.0, 1.00, 2.0])",
            Value::new(Raw::Array(vec![
                Value::decimal(1.into(), 0.into()).unwrap().raw().clone(),
                Value::decimal(2.into(), 0.into()).unwrap().raw().clone(),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.union([1, 2], [2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "[1, 2] | std.collection.union(right: [2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.union(right: [2, 3], left: [1, 2])",
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        ("std.collection.count([1, 2, 3])", Value::int(3.into())),
        ("[1, 2, 3] | std.collection.count()", Value::int(3.into())),
        (
            "std.collection.count(values: [1, 2, 3, 4])",
            Value::int(4.into()),
        ),
        (
            "std.collection.take([1, 2, 3], 2)",
            Value::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())])).unwrap(),
        ),
        (
            "[1, 2, 3] | std.collection.take(count: 0)",
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            "std.collection.take(count: 9, values: [1, 2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.drop([1, 2, 3], 1)",
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap(),
        ),
        (
            "[1, 2, 3] | std.collection.drop(count: 2)",
            Value::new(Raw::Array(vec![Raw::Int(3.into())])).unwrap(),
        ),
        (
            "std.collection.drop(count: 9, values: [1, 2, 3])",
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            "std.collection.partition([1, 2, 3, 4], value => value % 2 == 0)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            "[1, 2, 3] | std.collection.partition(predicate: value => value > 1)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(1.into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.partition(predicate: value => value >= 2, values: [1, 2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(1.into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.split_when([1, 2, 3, 4, 5], value => value % 2 == 0)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(4.into()), Raw::Int(5.into())]),
            ]))
            .unwrap(),
        ),
        (
            "[1, 2, 3] | std.collection.split_when(predicate: value => value == 1)",
            Value::new(Raw::Array(vec![Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ])]))
            .unwrap(),
        ),
        (
            "std.collection.split_when(predicate: value => value == 2, values: [1, 2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.group_by([31, 12, 22, 13, 33], value => value % 10)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Array(vec![Raw::Int(31.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(2.into()),
                    Raw::Array(vec![Raw::Int(12.into()), Raw::Int(22.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(3.into()),
                    Raw::Array(vec![Raw::Int(13.into()), Raw::Int(33.into())]),
                ]),
            ]))
            .unwrap(),
        ),
        (
            "[3, 1, 2, 4] | std.collection.group_by(key: value => value % 2)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(0.into()),
                    Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
                ]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.zip([1, 2, 3], [\"a\", \"b\"])",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.zip_exact([1, 2], [\"a\", \"b\"])",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.pairs([1, 2, 3])",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.window([1, 2, 3, 4, 5], 3, 2)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Int(2.into()),
                    Raw::Int(3.into()),
                ]),
                Raw::Array(vec![
                    Raw::Int(3.into()),
                    Raw::Int(4.into()),
                    Raw::Int(5.into()),
                ]),
            ]))
            .unwrap(),
        ),
        (
            "std.collection.window([1, 2, 3, 4], 3, 2)",
            Value::new(Raw::Array(vec![Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ])]))
            .unwrap(),
        ),
        (
            "[1, 2, 3] | std.collection.window(size: 2)",
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_first_returns_only_the_head_of_a_finite_list() {
    for (expression, expected) in [
        ("first([0, 1, 2])", Value::int(0.into())),
        ("std.collection.first([1, 2, 3])", Value::int(1.into())),
        ("std.collection.first([])", Value::new(Raw::Null).unwrap()),
        ("[3, 4, 5] | first()", Value::int(3.into())),
        ("[4, 5, 6] | std.collection.first()", Value::int(4.into())),
        ("first(rows: [7, 8, 9])", Value::int(7.into())),
        (
            "std.collection.first(rows: [8, 9, 10])",
            Value::int(8.into()),
        ),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn pick(rows: [Int]) = first(rows);",
            "pick([10, 11, 12])",
            Limits::default(),
        )
        .unwrap(),
        Value::int(10.into())
    );
}

#[test]
fn std_collection_first_is_callback_free_and_bounded() {
    for (expression, expected) in [
        ("std.collection.first(1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.first([1], value => value / 0)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            "std.collection.first(rows: [1], callback: value => value)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.first(values: [1])", "ORNA-EVAL-UNSUPPORTED"),
        ("std.collection.first()", "ORNA-EVAL-UNSUPPORTED"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            "std.collection.first([13])",
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::int(13.into())
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.first([13])",
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn root_first_dispatch_respects_local_function_and_relation_shadowing() {
    assert_eq!(
        call_module(
            "fn first(value: Int) = value + 100; fn run() = first(1);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            "if true { let first = value => value + 100; first(1) } else { 0 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        code(evaluate_expression(
            "Note.first()",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn std_collection_fallback_rejects_invalid_arguments_and_enforces_limits() {
    for (expression, expected) in [
        ("std.collection.chunk([1], 0)", "ORNA-EVAL-VALUE"),
        ("std.collection.chunk([1], -1)", "ORNA-EVAL-VALUE"),
        ("std.collection.flatten([1])", "ORNA-EVAL-TYPE"),
        ("std.collection.unique(1)", "ORNA-EVAL-TYPE"),
        ("std.collection.unique([1], 2)", "ORNA-EVAL-UNSUPPORTED"),
        ("std.collection.distinct(1)", "ORNA-EVAL-TYPE"),
        ("std.collection.distinct([1], 2)", "ORNA-EVAL-UNSUPPORTED"),
        (
            "std.collection.distinct([1.0f, 1.0f])",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.union(1, [2])", "ORNA-EVAL-TYPE"),
        ("std.collection.union([1])", "ORNA-EVAL-UNSUPPORTED"),
        (
            "std.collection.union(left: [1], right: [2], extra: [3])",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.count(1)", "ORNA-EVAL-TYPE"),
        ("std.collection.count([1], 2)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.count(values: [1], extra: 2)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.take(1, 1)", "ORNA-EVAL-TYPE"),
        ("std.collection.take([1], 1.0)", "ORNA-EVAL-TYPE"),
        ("std.collection.take([1], -1)", "ORNA-EVAL-VALUE"),
        (
            "std.collection.take([1], 1, extra: 2)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.drop(1, 1)", "ORNA-EVAL-TYPE"),
        ("std.collection.drop([1], 1.0)", "ORNA-EVAL-TYPE"),
        ("std.collection.drop([1], -1)", "ORNA-EVAL-VALUE"),
        (
            "std.collection.drop([1], 1, extra: 2)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            "std.collection.partition(1, value => true)",
            "ORNA-EVAL-TYPE",
        ),
        ("std.collection.partition([1], 1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.partition([1], value => 1)",
            "ORNA-EVAL-TYPE",
        ),
        (
            "std.collection.partition([1], () => true)",
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            "std.collection.partition([1], value => true, extra: value => true)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.filter(1, value => true)", "ORNA-EVAL-TYPE"),
        ("std.collection.filter([1], 1)", "ORNA-EVAL-TYPE"),
        ("std.collection.filter([1], value => 1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.filter([1], () => true)",
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            "std.collection.filter([1], value => true, extra: value => true)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            "std.collection.split_when(1, value => true)",
            "ORNA-EVAL-TYPE",
        ),
        ("std.collection.split_when([1], 1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.split_when([1], value => 1)",
            "ORNA-EVAL-TYPE",
        ),
        (
            "std.collection.split_when([1], () => true)",
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            "std.collection.split_when([1], value => true, extra: value => true)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            "std.collection.group_by(1, value => value)",
            "ORNA-EVAL-TYPE",
        ),
        ("std.collection.group_by([1], 1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.group_by([1], value => [value])",
            "ORNA-EVAL-TYPE",
        ),
        (
            "std.collection.group_by([1], value => value, extra: value => value)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("std.collection.zip_exact([1], [2, 3])", "ORNA-EVAL-VALUE"),
        ("std.collection.pairs(1)", "ORNA-EVAL-TYPE"),
        ("std.collection.window([1, 2], 0)", "ORNA-EVAL-VALUE"),
        ("std.collection.window([1, 2], 1, 0)", "ORNA-EVAL-VALUE"),
        ("std.collection.window([1, 2], 1, -1)", "ORNA-EVAL-VALUE"),
        (
            "std.collection.window([1, 2], 1, 1, 1)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "std.collection.unique([1, 2, 3])",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.distinct([1, 2, 3])",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.union([1, 2], [3])",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.count([1, 2, 3])",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.window([1, 2, 3], 2)",
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    let rows = Environment::from([(
        "rows".into(),
        Value::new(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Int(2.into()),
            Raw::Int(3.into()),
        ]))
        .unwrap(),
    )]);
    assert_eq!(
        evaluate_expression(
            "std.collection.take(rows, 1)",
            &rows,
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.take(rows, 2)",
            &rows,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            "std.collection.drop(rows, 1)",
            &rows,
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.drop(rows, 1)",
            &rows,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_partition_accepts_lawful_predicates_and_enforces_limits() {
    let source = "fn keep_even(value: Int) = value % 2 == 0; fn run() = std.collection.partition([1, 2, 3, 4], keep_even);";
    assert_eq!(
        call_module(source, "run()", Limits::default()).unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(3.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(call_module(
            "fn bad(value: Int, other: Int) = true; fn run() = std.collection.partition([1], bad);",
            "run()",
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.partition([1, 2, 3], value => true)",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_filter_accepts_direct_pipeline_and_named_calls() {
    let expected = Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())])).unwrap();
    assert_eq!(
        evaluate_expression(
            "std.collection.filter([1, 2, 3, 4], value => value % 2 == 0)",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        evaluate_expression(
            "[1, 2, 3, 4] | std.collection.filter(predicate: value => value % 2 == 0)",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        evaluate_expression(
            "std.collection.filter(predicate: value => value % 2 == 0, values: [1, 2, 3, 4])",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        call_module(
            "fn keep_even(value: Int) = value % 2 == 0; fn run() = std.collection.filter([1, 2, 3, 4], keep_even);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_filter_enforces_limits() {
    assert_eq!(
        code(evaluate_expression(
            "std.collection.filter([1, 2, 3], value => true)",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_map_preserves_order_for_direct_pipeline_named_and_function_calls() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(30.into()),
        Raw::Int(10.into()),
        Raw::Int(20.into()),
    ]))
    .unwrap();
    for expression in [
        "std.collection.map([3, 1, 2], value => value * 10)",
        "[3, 1, 2] | std.collection.map(transform: value => value * 10)",
        "std.collection.map(transform: value => value * 10, values: [3, 1, 2])",
        "map([3, 1, 2], value => value * 10)",
        "[3, 1, 2] | map(transform: value => value * 10)",
        "map(transform: value => value * 10, values: [3, 1, 2])",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn scale(value: Int) = value * 10; fn run() = std.collection.map([3, 1, 2], scale);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_flat_map_preserves_outer_and_inner_order_for_all_call_forms() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(3.into()),
        Raw::Int(13.into()),
        Raw::Int(1.into()),
        Raw::Int(11.into()),
        Raw::Int(2.into()),
        Raw::Int(12.into()),
    ]))
    .unwrap();
    for expression in [
        "std.collection.flat_map([3, 1, 2], value => [value, value + 10])",
        "[3, 1, 2] | std.collection.flat_map(transform: value => [value, value + 10])",
        "std.collection.flat_map(transform: value => [value, value + 10], values: [3, 1, 2])",
        "flat_map([3, 1, 2], value => [value, value + 10])",
        "[3, 1, 2] | flat_map(transform: value => [value, value + 10])",
        "flat_map(transform: value => [value, value + 10], values: [3, 1, 2])",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn expand(value: Int) = [value, value + 10]; fn run() = std.collection.flat_map([3, 1, 2], expand);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn root_map_names_remain_shadowable_by_admitted_functions_and_locals() {
    assert_eq!(
        call_module(
            "fn map(value: Int) = value + 100; fn run() = map(1);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            "if true { let map = value => value + 100; map(1) } else { 0 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
}

#[test]
fn std_collection_map_and_flat_map_propagate_callback_errors() {
    for expression in [
        "std.collection.map([1, 0, 2], value => 10 / value)",
        "std.collection.flat_map([1, 0, 2], value => [10 / value])",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-DIVIDE-BY-ZERO",
            "{expression}"
        );
    }
    assert_eq!(
        code(call_module(
            "fn bad(value: Int, other: Int) = value; fn run() = std.collection.map([1], bad);",
            "run()",
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
}

#[test]
fn std_collection_map_and_flat_map_reject_invalid_inputs_and_outputs() {
    for (expression, expected) in [
        ("std.collection.map(1, value => value)", "ORNA-EVAL-TYPE"),
        ("std.collection.map([1], 1)", "ORNA-EVAL-TYPE"),
        ("std.collection.map([1])", "ORNA-EVAL-UNSUPPORTED"),
        ("std.collection.map([1], () => 1)", "ORNA-EVAL-ARGUMENT"),
        (
            "std.collection.flat_map(1, value => [value])",
            "ORNA-EVAL-TYPE",
        ),
        ("std.collection.flat_map([1], 1)", "ORNA-EVAL-TYPE"),
        (
            "std.collection.flat_map([1], value => value)",
            "ORNA-EVAL-TYPE",
        ),
        ("std.collection.flat_map([1])", "ORNA-EVAL-UNSUPPORTED"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_collection_map_and_flat_map_enforce_collection_limits() {
    assert_eq!(
        code(evaluate_expression(
            "std.collection.map([1, 2, 3], value => value)",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.flat_map([1, 2], value => [value, value])",
            &Environment::new(),
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_split_when_accepts_functions_and_enforces_limits() {
    let source = "fn boundary(value: Int) = value % 2 == 0; fn run() = std.collection.split_when([1, 2, 3, 4], boundary);";
    assert_eq!(
        call_module(source, "run()", Limits::default()).unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            Raw::Array(vec![Raw::Int(4.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(call_module(
            "fn bad(value: Int, other: Int) = true; fn run() = std.collection.split_when([1], bad);",
            "run()",
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.split_when([1, 2, 3], value => true)",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_group_by_accepts_functions_and_enforces_limits() {
    let source = "fn parity(value: Int) = value % 2; fn run() = std.collection.group_by([3, 2, 1, 4], parity);";
    assert_eq!(
        call_module(source, "run()", Limits::default()).unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            ]),
            Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
            ]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.group_by([1, 2, 3], value => value)",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
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

#[test]
fn executes_function_and_finite_for_control_transfers() {
    assert_eq!(
        invoke(
            "fn early() { return 7; 9 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(7.into())
    );
    assert_eq!(
        evaluate(
            "if true { let total = 0; for value in [1, 2, 3, 4] { total += value; if value == 2 { break; }; }; total }"
        ),
        Value::int(3.into())
    );
    assert_eq!(
        evaluate(
            "if true { let total = 0; for value in 1..=4 { if value == 2 { continue; }; total += value; }; total }"
        ),
        Value::int(8.into())
    );
    assert_eq!(
        evaluate(
            "if true { let total = 0; for outer in [1, 2] { for inner in [1, 2] { if inner == 1 { continue; }; total += outer; break; }; }; total }"
        ),
        Value::int(3.into())
    );
}

#[test]
fn transfer_boundaries_reject_loop_transfers_from_a_called_lambda() {
    assert_eq!(
        code(call_module(
            "fn outer() { for value in [1] { let stop = () => { break; }; stop(); }; 0 }",
            "outer()",
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { return 1; 0 }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { for value in [1] { break 1; }; 0 }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn integer_ranges_are_canonical_membership_values_and_finite_iterables() {
    let half_open = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(1.into())])),
            ),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    assert_eq!(evaluate("1..5"), half_open);
    assert_eq!(evaluate("1 in 1..5"), Value::new(Raw::Bool(true)).unwrap());
    assert_eq!(evaluate("5 in 1..5"), Value::new(Raw::Bool(false)).unwrap());
    assert_eq!(evaluate("5 in 1..=5"), Value::new(Raw::Bool(true)).unwrap());
    assert_eq!(evaluate("-1 in ..5"), Value::new(Raw::Bool(true)).unwrap());
    assert_eq!(evaluate("5 in 5.."), Value::new(Raw::Bool(true)).unwrap());
    assert_eq!(
        evaluate("5..1"),
        Value::new(Raw::Tag(
            60019,
            Box::new(Raw::Array(vec![
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
                ),
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(1.into())])),
                ),
                Raw::Bool(false),
            ])),
        ))
        .unwrap()
    );
    assert_eq!(
        evaluate("if true { let total = 0; for value in 1..5 { total += value; }; total }"),
        Value::int(10.into())
    );
    assert_eq!(
        evaluate("if true { let total = 0; for value in 1..=5 { total += value; }; total }"),
        Value::int(15.into())
    );
    assert_eq!(
        evaluate("if true { let total = 0; for value in 5..1 { total += value; }; total }"),
        Value::int(0.into())
    );
}

#[test]
fn integer_ranges_reject_unsupported_forms_and_obey_finite_limits() {
    for source in ["1.0..5.0", "1 in 1.0..5.0", "1 in 1..5.0"] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "if true { for value in 0..3 { value }; 0 }",
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    let unbounded = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    let environment = Environment::from([("range".into(), unbounded)]);
    assert_eq!(
        evaluate_expression("-1 in range", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { for value in range { value }; 0 }",
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate("..5"),
        Value::new(Raw::Tag(
            60019,
            Box::new(Raw::Array(vec![
                Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
                ),
                Raw::Bool(false),
            ])),
        ))
        .unwrap()
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
