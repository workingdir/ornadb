use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Limits, NominalDefinition, NominalDefinitions,
    NominalField, PureFunction, StepBudget, evaluate_expression, evaluate_function,
    evaluate_parsed, evaluate_parsed_with_nominals, evaluate_repl,
    evaluate_with_functions_and_nominals, invoke_named, invoke_named_with_effects,
    invoke_named_with_nominals,
};
use orna_syntax_v1::{
    AssignmentOperator, AssignmentTarget, Expr, NameSegment, Pattern, RecordField, Statement,
    SyntaxSpan, TokenKind, lex,
};
use orna_value_v1::{CANONICAL_NAN_BITS, Raw, Value};

fn evaluate(source: &str) -> Value {
    evaluate_expression(source, &Environment::new(), Limits::default())
        .unwrap_or_else(|error| panic!("{}: {}", source, error.code()))
}
fn code(result: Result<Value, EvaluationError>) -> String {
    result.unwrap_err().code().to_owned()
}

fn float_rows(bits: &[u64]) -> Value {
    Value::new(Raw::Array(bits.iter().copied().map(Raw::Float).collect())).unwrap()
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

fn parsed_expression(source: &str) -> Expr {
    let parsed = orna_syntax_v1::parse_expression(source);
    assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    parsed.value
}

fn nominal_field(name: &str, public: bool, default: Option<&str>) -> NominalField {
    match (public, default) {
        (true, Some(default)) => NominalField::public_with_default(
            field_id_bytes(name),
            name,
            parsed_expression(default),
        ),
        (true, None) => NominalField::public(field_id_bytes(name), name),
        (false, Some(default)) => NominalField::private_with_default(
            field_id_bytes(name),
            name,
            parsed_expression(default),
        ),
        (false, None) => NominalField::private(field_id_bytes(name), name),
    }
}

fn object_id_bytes(name: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, byte) in name.bytes().take(16).enumerate() {
        bytes[index] = byte;
    }
    bytes
}

fn field_id_bytes(name: &str) -> [u8; 16] {
    object_id_bytes(&format!("field:{name}"))
}

fn type_id_raw(name: &str) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(object_id_bytes(name).to_vec())))
}

fn field_id_raw(name: &str) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(field_id_bytes(name).to_vec())))
}

fn nominal_definitions(
    path: &str,
    type_id: &str,
    owner: Option<&str>,
    fields: Vec<NominalField>,
) -> NominalDefinitions {
    NominalDefinitions::from([(
        path.into(),
        NominalDefinition::new(object_id_bytes(type_id), owner.map(str::to_owned), fields),
    )])
}

fn nominal_parts(value: &orna_foundation_v1::CanonicalValue) -> (&Raw, &[Raw]) {
    let Raw::Tag(60009, body) = value.raw() else {
        panic!("expected nominal value, got {value:?}");
    };
    let Raw::Array(parts) = body.as_ref() else {
        panic!("expected nominal payload");
    };
    let [type_id, Raw::Array(fields)] = parts.as_slice() else {
        panic!("expected nominal identity and fields");
    };
    (type_id, fields.as_slice())
}

#[test]
fn nominal_construction_materializes_supplied_and_default_fields_in_declaration_order() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("left", true, None),
            nominal_field("right", true, Some("left + 1")),
            nominal_field("tail", true, Some("right + 1")),
        ],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression("Thing { left: 3, right: left + 1 }"),
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("nominal construction should evaluate");
    let (type_id, fields) = nominal_parts(&result);

    assert_eq!(type_id, &type_id_raw("stable.Thing"));
    assert_eq!(
        fields,
        &[
            Raw::Array(vec![field_id_raw("left"), Raw::Int(3.into())]),
            Raw::Array(vec![field_id_raw("right"), Raw::Int(4.into())]),
            Raw::Array(vec![field_id_raw("tail"), Raw::Int(5.into())]),
        ]
    );
}

#[test]
fn nominal_supplied_fields_evaluate_in_written_order_before_canonical_serialization() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("second", true, None),
            nominal_field("first", true, None),
        ],
    );
    let span = SyntaxSpan::new(0, 0);
    let counter = || Expr::Name {
        text: "counter".into(),
        span: span.clone(),
    };
    let increment = || Expr::Binary {
        lhs: Box::new(counter()),
        op: "+".into(),
        rhs: Box::new(Expr::Literal {
            text: "1".into(),
            kind: orna_syntax_v1::LiteralKind::Integer,
            span: span.clone(),
        }),
        span: span.clone(),
    };
    let counted = || Expr::Block {
        statements: vec![Statement::Assignment {
            target: AssignmentTarget::Name {
                name: "counter".into(),
                span: span.clone(),
            },
            operator: AssignmentOperator::Set,
            value: increment(),
            span: span.clone(),
        }],
        tail: Some(Box::new(counter())),
        span: span.clone(),
    };
    let body = Expr::Block {
        statements: vec![Statement::Let {
            pattern: Pattern::Name("counter".into(), span.clone()),
            annotation: None,
            value: Expr::Literal {
                text: "0".into(),
                kind: orna_syntax_v1::LiteralKind::Integer,
                span: span.clone(),
            },
            span: span.clone(),
        }],
        tail: Some(Box::new(Expr::Nominal {
            path: vec![NameSegment {
                text: "Thing".into(),
                span: span.clone(),
            }],
            fields: vec![
                RecordField {
                    name: "first".into(),
                    value: counted(),
                    span: span.clone(),
                },
                RecordField {
                    name: "second".into(),
                    value: counted(),
                    span: span.clone(),
                },
            ],
            span: span.clone(),
        })),
        span,
    };
    let functions = BTreeMap::from([(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body,
            environment: Environment::new(),
        },
    )]);
    let result = invoke_named_with_nominals(
        "run",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("supplied fields should evaluate in source order");
    let (_, fields) = nominal_parts(&result);
    let values = fields
        .iter()
        .map(|field| {
            let Raw::Array(parts) = field else {
                panic!("expected nominal field entry");
            };
            let [key, value] = parts.as_slice() else {
                panic!("expected nominal field entry");
            };
            let name = ["first", "second"]
                .into_iter()
                .find(|name| field_id_raw(name) == *key)
                .expect("known nominal field id");
            (name.to_owned(), value.clone())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(values.get("first"), Some(&Raw::Int(1.into())));
    assert_eq!(values.get("second"), Some(&Raw::Int(2.into())));
}

#[test]
fn nominal_supplied_values_bypass_omitted_defaults() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, Some("1 / 0"))],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression("Thing { value: 9 }"),
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("a supplied field must not evaluate its default");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("value"), Raw::Int(9.into())])]
    );
}

#[test]
fn external_nominal_public_default_runs_in_declaration_owner_namespace() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("value", true, Some("helper()"))],
    );
    let functions = BTreeMap::from([(
        "vault.helper".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression("7"),
            environment: Environment::new(),
        },
    )]);
    let result = evaluate_with_functions_and_nominals(
        &parsed_expression("vault.Vault {}"),
        &Environment::new(),
        &functions,
        &definitions,
        Limits::default(),
    )
    .expect("public defaults must execute in their declaration namespace");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("value"), Raw::Int(7.into())])]
    );
}

#[test]
fn nominal_defaults_run_once_in_declaration_order() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("first", true, Some("1")),
            nominal_field("second", true, Some("first + 1")),
        ],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression("Thing {}"),
        &Environment::new(),
        &definitions,
        Limits {
            max_steps: 5,
            ..Limits::default()
        },
    )
    .expect("each declaration default should execute once");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[
            Raw::Array(vec![field_id_raw("first"), Raw::Int(1.into())]),
            Raw::Array(vec![field_id_raw("second"), Raw::Int(2.into())]),
        ]
    );
}

#[test]
fn nominal_construction_rejects_duplicate_unknown_missing_and_failing_fields() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("required", true, None),
            nominal_field("failing", true, Some("1 / 0")),
        ],
    );
    for source in ["Thing { required: 1, required: 2 }", "Thing { unknown: 1 }"] {
        assert_eq!(
            evaluate_parsed_with_nominals(
                &parsed_expression(source),
                &Environment::new(),
                &definitions,
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-EVAL-ARGUMENT",
            "{source}"
        );
    }
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression("Thing {}"),
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression("Thing { required: 1 }"),
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn external_nominal_construction_rejects_private_fields_but_owner_namespace_can_construct() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let expression = parsed_expression("vault.Vault { secret: 7 }");
    assert_eq!(
        evaluate_parsed_with_nominals(
            &expression,
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let defaulted_private = nominal_definitions(
        "vault.Defaulted",
        "stable.Defaulted",
        Some("vault"),
        vec![nominal_field("secret", false, Some("7"))],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression("vault.Defaulted {}"),
        &Environment::new(),
        &defaulted_private,
        Limits::default(),
    )
    .expect("an omitted private field with a declaration default is admissible");
    let (type_id, fields) = nominal_parts(&result);
    assert_eq!(type_id, &type_id_raw("stable.Defaulted"));
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("secret"), Raw::Int(7.into())])]
    );

    let mut definitions = definitions;
    definitions.insert(
        "Vault".into(),
        NominalDefinition::new(
            object_id_bytes("stable.ShortVault"),
            None,
            vec![nominal_field("other", true, Some("1"))],
        ),
    );

    let functions = std::collections::BTreeMap::from([
        (
            "vault.make".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression("vault.inner()"),
                environment: Environment::new(),
            },
        ),
        (
            "vault.inner".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression("Vault { secret: 7 }"),
                environment: Environment::new(),
            },
        ),
    ]);
    let result = invoke_named_with_nominals(
        "vault.make",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("the owning namespace may construct private fields");
    let (type_id, fields) = nominal_parts(&result);
    assert_eq!(type_id, &type_id_raw("stable.Vault"));
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("secret"), Raw::Int(7.into())])]
    );

    let root_owned = nominal_definitions(
        "RootThing",
        "stable.RootThing",
        None,
        vec![nominal_field("secret", false, None)],
    );
    evaluate_parsed_with_nominals(
        &parsed_expression("RootThing { secret: 7 }"),
        &Environment::new(),
        &root_owned,
        Limits::default(),
    )
    .expect("root-owned construction uses the root namespace");
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

struct BudgetedEffects;

impl EffectHandler for BudgetedEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        NoteEffects::default().handle(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
        budget: &mut StepBudget,
    ) -> Result<Option<Value>, EvaluationError> {
        budget.debit(1)?;
        self.handle(callee, arguments)
    }
}

#[test]
fn effect_budget_hook_shares_activation_steps_and_exhausts() {
    let functions = functions_from_source("fn entry() = Note.insert(1);");
    let mut effects = BudgetedEffects;

    assert_eq!(
        code(invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            },
            &mut effects,
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn legacy_effect_handler_keeps_existing_budget_behavior() {
    let functions = functions_from_source("fn entry() = Note.insert(1);");
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            },
            &mut effects,
        )
        .unwrap(),
        Value::int(1.into())
    );
    assert_eq!(effects.calls, vec![vec![Value::int(1.into())]]);
}

#[test]
fn fail_reemits_the_original_error_instead_of_returning_a_value() {
    let result = evaluate_expression(
        "(1 / 0) |? (failure => fail(failure))",
        &Environment::new(),
        Limits::default(),
    );

    assert_eq!(code(result), "ORNA-EVAL-DIVIDE-BY-ZERO");
}

#[test]
fn fail_from_a_recovery_handler_reaches_the_next_recovery_boundary() {
    assert_eq!(
        evaluate("(1 / 0) |? (failure => fail(failure)) |? (failure => 7)"),
        Value::int(7.into())
    );
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
fn dynamic_field_calls_evaluate_callee_before_effectful_arguments() {
    let functions = functions_from_source(
        "fn make() = Note.insert(1); fn run() = make().field(Note.insert(2));",
    );
    let mut effects = NoteEffects::default();

    assert_eq!(
        code(invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )),
        "ORNA-EVAL-TYPE"
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
fn std_collection_sum_accepts_direct_named_pipeline_and_function_calls() {
    let expected = Value::int(6.into());
    for expression in [
        "sum([3, 1, 2])",
        "std.collection.sum([3, 1, 2])",
        "sum(rows: [3, 1, 2])",
        "std.collection.sum(rows: [3, 1, 2])",
        "[3, 1, 2] | sum",
        "[3, 1, 2] | sum()",
        "[3, 1, 2] | std.collection.sum",
        "[3, 1, 2] | std.collection.sum()",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn total(rows: [Int]) = sum(rows);",
            "total([3, 1, 2])",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_sum_accumulates_exactly_in_order_and_returns_integer_zero() {
    assert_eq!(evaluate("sum([])"), Value::int(0.into()),);
    assert_eq!(
        evaluate("sum([9007199254740993, 1])"),
        Value::int(9007199254740994_u64.into()),
    );

    let limited = Limits {
        max_integer_digits: 3,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            "sum([999, 1, -99, -99, -99, -99, -99, -99, -99, -99, -99, -10])",
            &Environment::new(),
            limited,
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            "sum([999, -99, 1, -99, -99, -99, -99, -99, -99, -99, -99, -99, -10])",
            &Environment::new(),
            limited,
        )
        .unwrap(),
        Value::int(0.into())
    );
}

#[test]
fn std_collection_decimal_sum_preserves_exact_canonical_arithmetic() {
    let large_coefficient = BigInt::parse_bytes(b"12345678901234567891", 10).unwrap();
    for (expression, expected) in [
        (
            "sum([1.20, 2.003])",
            Value::decimal(3203.into(), (-3).into()).unwrap(),
        ),
        (
            "sum([12345678901234567890.1, 0.9])",
            Value::decimal(large_coefficient, 0.into()).unwrap(),
        ),
        (
            "sum([999.999, -999.99, 0.001])",
            Value::decimal(1.into(), (-2).into()).unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_decimal_sum_rejects_mixed_numeric_kinds() {
    assert_eq!(
        code(evaluate_expression(
            "sum([1.0, 2])",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn std_collection_sum_rejects_unsupported_numeric_kinds_and_shapes() {
    for expression in ["sum([1, true])", "sum(values: [1])"] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "sum(1)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            "sum()",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let currency = Raw::Tag(37, Box::new(Raw::Bytes(vec![0; 16])));
    let affine = Raw::Tag(
        60006,
        Box::new(Raw::Array(vec![Raw::Int(1.into()), currency.clone()])),
    );
    let money = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
            ),
            currency,
        ])),
    );
    for raw in [affine, money] {
        let environment =
            Environment::from([("values".into(), Value::new(Raw::Array(vec![raw])).unwrap())]);
        assert_eq!(
            code(evaluate_expression(
                "sum(values)",
                &environment,
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
}

#[test]
fn std_collection_float_sum_accepts_direct_named_pipeline_and_function_calls() {
    let expected = Value::float_bits(3.75f64.to_bits());
    for expression in [
        "sum([1.5f, 2.25f])",
        "std.collection.sum([1.5f, 2.25f])",
        "sum(rows: [1.5f, 2.25f])",
        "std.collection.sum(rows: [1.5f, 2.25f])",
        "[1.5f, 2.25f] | sum",
        "[1.5f, 2.25f] | sum()",
        "[1.5f, 2.25f] | std.collection.sum",
        "[1.5f, 2.25f] | std.collection.sum()",
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }

    assert_eq!(
        call_module(
            "fn total(rows: [Float]) = sum(rows);",
            "total([1.5f, 2.25f])",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_float_sum_folds_left_to_right_and_propagates_canonical_nan() {
    let rows = float_rows(&[
        10_000_000_000_000_000.0f64.to_bits(),
        (-10_000_000_000_000_000.0f64).to_bits(),
        1.0f64.to_bits(),
    ]);
    let environment = Environment::from([("rows".into(), rows)]);
    assert_eq!(
        evaluate_expression("sum(rows)", &environment, Limits::default()).unwrap(),
        Value::float_bits(1.0f64.to_bits())
    );

    let nan_environment = Environment::from([(
        "rows".into(),
        float_rows(&[1.0f64.to_bits(), CANONICAL_NAN_BITS, 2.0f64.to_bits()]),
    )]);
    for expression in [
        "sum(rows)",
        "std.collection.sum(rows)",
        "sum(rows: rows)",
        "std.collection.sum(rows: rows)",
        "rows | sum",
        "rows | sum()",
        "rows | std.collection.sum",
        "rows | std.collection.sum()",
    ] {
        assert_eq!(
            evaluate_expression(expression, &nan_environment, Limits::default()).unwrap(),
            Value::float_bits(CANONICAL_NAN_BITS),
            "{expression}"
        );
    }
}

#[test]
fn std_collection_float_min_and_max_use_total_order_and_ordinary_equality_separately() {
    let negative_zero = Value::float_bits((-0.0f64).to_bits());
    let positive_zero = Value::float_bits(0.0f64.to_bits());
    let expected_min = Value::option(Some(negative_zero.clone())).unwrap();
    let expected_max = Value::option(Some(positive_zero.clone())).unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in [
            format!("{name}([-0.0f, 0.0f])"),
            format!("std.collection.{name}([-0.0f, 0.0f])"),
            format!("{name}(rows: [-0.0f, 0.0f])"),
            format!("std.collection.{name}(rows: [-0.0f, 0.0f])"),
            format!("[-0.0f, 0.0f] | {name}"),
            format!("[-0.0f, 0.0f] | {name}()"),
            format!("[-0.0f, 0.0f] | std.collection.{name}"),
            format!("[-0.0f, 0.0f] | std.collection.{name}()"),
        ] {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = "fn lowest(rows: [Float]) = min(rows); fn highest(rows: [Float]) = std.collection.max(rows);";
    assert_eq!(
        call_module(source, "lowest([-3.0f, 2.0f])", Limits::default()).unwrap(),
        Value::option(Some(Value::float_bits((-3.0f64).to_bits()))).unwrap()
    );
    assert_eq!(
        call_module(source, "highest([-3.0f, 2.0f])", Limits::default()).unwrap(),
        Value::option(Some(Value::float_bits(2.0f64.to_bits()))).unwrap()
    );

    let environment = Environment::from([
        ("negative_zero".into(), negative_zero),
        ("positive_zero".into(), positive_zero),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression(
            "negative_zero == positive_zero",
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression("nan == nan", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
}

#[test]
fn std_collection_float_min_and_max_propagate_nan_and_return_empty_identity() {
    let environment = Environment::from([(
        "rows".into(),
        float_rows(&[(-1.0f64).to_bits(), CANONICAL_NAN_BITS, 2.0f64.to_bits()]),
    )]);
    let expected = Value::option(Some(Value::float_bits(CANONICAL_NAN_BITS))).unwrap();
    for (name, expected) in [("min", expected.clone()), ("max", expected)] {
        for expression in [
            format!("{name}(rows)"),
            format!("std.collection.{name}(rows)"),
            format!("{name}(rows: rows)"),
            format!("std.collection.{name}(rows: rows)"),
            format!("rows | {name}"),
            format!("rows | {name}()"),
            format!("rows | std.collection.{name}"),
            format!("rows | std.collection.{name}()"),
        ] {
            assert_eq!(
                evaluate_expression(&expression, &environment, Limits::default()).unwrap(),
                expected,
                "{expression}"
            );
        }
    }

    for expression in [
        "min([])",
        "max([])",
        "std.collection.min(rows: [])",
        "std.collection.max(rows: [])",
        "[] | min()",
        "[] | max()",
    ] {
        assert_eq!(
            evaluate(expression),
            Value::new(Raw::Null).unwrap(),
            "{expression}"
        );
    }
}

#[test]
fn std_collection_float_aggregates_reject_mixed_inputs_callbacks_and_limits() {
    for expression in [
        "sum([1.0f, 2])",
        "sum([1, 2.0f])",
        "min([1.0f, 2])",
        "std.collection.max([1, 2.0f])",
        "sum([1.0f], value => value)",
        "std.collection.min(rows: [1.0f], callback: value => value)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            "sum([1.0f, 1.0f / 0.0f])",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );

    for (expression, limits) in [
        (
            "sum([1.0f, 2.0f])",
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        ),
        (
            "std.collection.max([1.0f])",
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(expression, &Environment::new(), limits)),
            "ORNA-EVAL-LIMIT",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_date_min_and_max_accept_all_call_forms_and_preserve_order() {
    let values = "[2024-02-29, 2024-01-01, 2024-12-31]";
    let expected_min = Value::option(Some(evaluate("2024-01-01"))).unwrap();
    let expected_max = Value::option(Some(evaluate("2024-12-31"))).unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in [
            format!("{name}({values})"),
            format!("std.collection.{name}({values})"),
            format!("{name}(rows: {values})"),
            format!("std.collection.{name}(rows: {values})"),
            format!("{values} | {name}"),
            format!("{values} | {name}()"),
            format!("{values} | std.collection.{name}"),
            format!("{values} | std.collection.{name}()"),
        ] {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = "fn earliest(rows: [Date]) = min(rows); fn latest(rows: [Date]) = std.collection.max(rows);";
    assert_eq!(
        call_module(
            source,
            "earliest([2024-03-01, 2024-02-01])",
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(evaluate("2024-02-01"))).unwrap()
    );
    assert_eq!(
        call_module(
            source,
            "latest([2024-03-01, 2024-02-01])",
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(evaluate("2024-03-01"))).unwrap()
    );
}

#[test]
fn std_collection_instant_min_and_max_use_normalized_utc_order() {
    let values =
        "[2024-02-29T05:30:00+05:30, 2024-02-29T00:00:00Z, 2024-02-28T23:59:59.999999999Z]";
    let expected_min = Value::option(Some(evaluate("2024-02-28T23:59:59.999999999Z"))).unwrap();
    let expected_max = Value::option(Some(evaluate("2024-02-29T05:30:00+05:30"))).unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in [
            format!("{name}({values})"),
            format!("std.collection.{name}({values})"),
            format!("{name}(rows: {values})"),
            format!("std.collection.{name}(rows: {values})"),
            format!("{values} | {name}"),
            format!("{values} | {name}()"),
            format!("{values} | std.collection.{name}"),
            format!("{values} | std.collection.{name}()"),
        ] {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source =
        "fn earliest(rows: [Instant]) = min(rows); fn latest(rows: [Instant]) = max(rows);";
    assert_eq!(
        call_module(
            source,
            "earliest([1970-01-01T00:00:01Z, 1970-01-01T00:00:00Z])",
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(evaluate("1970-01-01T00:00:00Z"))).unwrap()
    );
    assert_eq!(
        call_module(
            source,
            "latest([1970-01-01T00:00:00Z, 1970-01-01T01:00:00+01:00])",
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(evaluate("1970-01-01T00:00:00Z"))).unwrap()
    );
}

#[test]
fn std_collection_temporal_min_and_max_keep_empty_null_and_reject_mixed_types() {
    for expression in [
        "min([])",
        "max([])",
        "std.collection.min(rows: [])",
        "std.collection.max(rows: [])",
        "[] | min()",
        "[] | max()",
    ] {
        assert_eq!(
            evaluate(expression),
            Value::new(Raw::Null).unwrap(),
            "{expression}"
        );
    }

    for expression in [
        "min([2024-01-01, 1])",
        "max([1, 2024-01-01])",
        "std.collection.min([1970-01-01T00:00:00Z, 1])",
        "std.collection.max([1, 1970-01-01T00:00:00Z])",
        "min([2024-01-01, 1970-01-01T00:00:00Z])",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_min_and_max_accept_integer_lists_in_all_call_forms() {
    let values = "[9007199254740993, -9007199254740993, 7, -9007199254740993]";
    let expected_min = Value::option(Some(Value::int((-9007199254740993_i64).into()))).unwrap();
    let expected_max = Value::option(Some(Value::int(9007199254740993_i64.into()))).unwrap();

    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in [
            format!("{name}({values})"),
            format!("std.collection.{name}({values})"),
            format!("{name}(rows: {values})"),
            format!("std.collection.{name}(rows: {values})"),
            format!("{values} | {name}"),
            format!("{values} | {name}()"),
            format!("{values} | std.collection.{name}"),
            format!("{values} | std.collection.{name}()"),
        ] {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source =
        "fn lowest(rows: [Int]) = min(rows); fn highest(rows: [Int]) = std.collection.max(rows);";
    assert_eq!(
        call_module(source, "lowest([3, 1, 2])", Limits::default()).unwrap(),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    assert_eq!(
        call_module(source, "highest([3, 1, 2])", Limits::default()).unwrap(),
        Value::option(Some(Value::int(3.into()))).unwrap()
    );
}

#[test]
fn std_collection_min_and_max_use_exact_integer_order_and_first_ties() {
    assert_eq!(
        evaluate("min([5, 1, 1, 9])"),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    assert_eq!(
        evaluate("max([5, 9, 9, 1])"),
        Value::option(Some(Value::int(9.into()))).unwrap()
    );
    assert_eq!(evaluate("min([])"), Value::new(Raw::Null).unwrap());
    assert_eq!(evaluate("max([])"), Value::new(Raw::Null).unwrap());
    assert_eq!(evaluate("[] | min()"), Value::new(Raw::Null).unwrap());
    assert_eq!(
        evaluate("std.collection.max(rows: [])"),
        Value::new(Raw::Null).unwrap()
    );
}

#[test]
fn std_collection_min_and_max_optional_results_match_some_null_and_coalesce() {
    for (expression, expected) in [
        (
            "case min([3, 1, 2]) { Some(value): value, null: 0 }",
            Value::int(1.into()),
        ),
        (
            "case max([3, 1, 2]) { Some(value): value, null: 0 }",
            Value::int(3.into()),
        ),
        (
            "case min([]) { Some(value): value, null: 10 }",
            Value::int(10.into()),
        ),
        (
            "case max([]) { Some(value): value, null: 10 }",
            Value::int(10.into()),
        ),
        ("min([3, 1, 2]) ?? 10", Value::int(1.into())),
        ("max([3, 1, 2]) ?? 10", Value::int(3.into())),
        ("min([]) ?? 10", Value::int(10.into())),
        ("max([]) ?? 10", Value::int(10.into())),
        ("min([3, 1, 2]) ?? (1 / 0)", Value::int(1.into())),
        ("max([3]) ?? (1 / 0)", Value::int(3.into())),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_min_and_max_fail_closed_for_unsupported_kinds_shapes_and_limits() {
    for expression in [
        "min([1.0])",
        "std.collection.max([1.0])",
        "std.collection.max([1, true])",
        "min(1)",
        "std.collection.max(1)",
        "std.collection.max()",
        "std.collection.min(values: [1])",
        "min([1], value => value / 0)",
    ] {
        let expected = if matches!(expression, "min(1)" | "std.collection.max(1)") {
            "ORNA-EVAL-TYPE"
        } else {
            "ORNA-EVAL-UNSUPPORTED"
        };
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

    let currency = Raw::Tag(37, Box::new(Raw::Bytes(vec![0; 16])));
    let affine = Raw::Tag(
        60006,
        Box::new(Raw::Array(vec![Raw::Int(1.into()), currency.clone()])),
    );
    let money = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
            ),
            currency,
        ])),
    );
    for raw in [affine, money] {
        let environment =
            Environment::from([("rows".into(), Value::new(Raw::Array(vec![raw])).unwrap())]);
        for operation in ["min", "max"] {
            assert_eq!(
                code(evaluate_expression(
                    &format!("{operation}(rows)"),
                    &environment,
                    Limits::default(),
                )),
                "ORNA-EVAL-UNSUPPORTED"
            );
        }
    }

    for (expression, limits) in [
        (
            "min([3, 2, 1])",
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        ),
        (
            "std.collection.max([1])",
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        ),
        (
            "min([1000])",
            Limits {
                max_integer_digits: 3,
                ..Limits::default()
            },
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(expression, &Environment::new(), limits)),
            "ORNA-EVAL-LIMIT",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_one_accepts_predicate_free_direct_pipeline_named_and_function_calls() {
    let expected = Value::int(7.into());
    for expression in [
        "one([7])",
        "std.collection.one([7])",
        "[7] | one()",
        "[7] | std.collection.one()",
        "one(rows: [7])",
        "std.collection.one(rows: [7])",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn pick(rows: [Int]) = one(rows);",
            "pick([7])",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_one_accepts_predicate_overloads_in_input_order() {
    let expected = Value::int(2.into());
    for expression in [
        "one([1, 2, 3], value => value == 2)",
        "std.collection.one([1, 2, 3], value => value == 2)",
        "[1, 2, 3] | one(predicate: value => value == 2)",
        "[1, 2, 3] | std.collection.one(predicate: value => value == 2)",
        "one(rows: [1, 2, 3], predicate: value => value == 2)",
        "std.collection.one(predicate: value => value == 2, rows: [1, 2, 3])",
        "one([1, 2, 3], predicate: value => value == 2)",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn is_two(value: Int) = value == 2; fn pick(rows: [Int]) = one(rows, is_two);",
            "pick([1, 2, 3])",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        call_module(
            "fn is_two(value: Int) = value == 2; fn pick(rows: [Int]) = one(predicate: is_two, rows: rows);",
            "pick([1, 2, 3])",
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_one_distinguishes_zero_and_multiple_matches() {
    for expression in [
        "one([])",
        "std.collection.one([])",
        "[] | one()",
        "one(rows: [])",
        "one([1, 2], value => value > 3)",
        "[1, 2] | std.collection.one(predicate: value => value > 3)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-RELATION-ONE-ZERO",
            "{expression}"
        );
    }
    for expression in [
        "one([1, 2])",
        "std.collection.one([1, 2])",
        "[1, 2] | one()",
        "one(rows: [1, 2])",
        "one([1, 2], value => value > 0)",
        "std.collection.one(predicate: value => value > 0, rows: [1, 2])",
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-RELATION-ONE-MULTIPLE",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_one_evaluates_predicates_in_order_and_stops_after_second_match() {
    assert_eq!(
        code(evaluate_expression(
            "one([0, 2, 3], value => if value == 0 { 1 / 0 == 0 } else { value > 1 })",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            "one([1, 0, 2], value => 1 / value == 1)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            "one([1, 2, 3], value => if value < 3 { true } else { 1 / 0 == 0 })",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-RELATION-ONE-MULTIPLE"
    );
}

#[test]
fn std_collection_one_rejects_invalid_inputs_propagates_callback_failures_and_keeps_limits() {
    for (expression, expected) in [
        ("one()", "ORNA-EVAL-UNSUPPORTED"),
        ("one(1)", "ORNA-EVAL-TYPE"),
        ("one([1], 1)", "ORNA-EVAL-TYPE"),
        ("one([1], value => 1)", "ORNA-EVAL-TYPE"),
        ("one([1], () => true)", "ORNA-EVAL-ARGUMENT"),
        (
            "one([1], value => value, value => value)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        ("one(values: [1])", "ORNA-EVAL-UNSUPPORTED"),
        ("one(rows: [1], rows: [2])", "ORNA-EVAL-UNSUPPORTED"),
        ("[1] | one(1)", "ORNA-EVAL-TYPE"),
        ("[1] | one(rows: [2])", "ORNA-EVAL-UNSUPPORTED"),
        (
            "std.collection.one(predicate: value => true)",
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            "one([1, 0], value => 1 / value == 1)",
            "ORNA-EVAL-DIVIDE-BY-ZERO",
        ),
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
            "one([13])",
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::int(13.into())
    );
    for limits in [
        Limits {
            max_collection_items: 2,
            ..Limits::default()
        },
        Limits {
            max_steps: 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            code(evaluate_expression(
                "one([1, 2, 3])",
                &Environment::new(),
                limits
            )),
            "ORNA-EVAL-LIMIT"
        );
    }
}

#[test]
fn std_collection_every_and_exists_accept_all_call_forms_and_function_callbacks() {
    let true_value = Value::new(Raw::Bool(true)).unwrap();
    let false_value = Value::new(Raw::Bool(false)).unwrap();
    for expression in [
        "every([1, 2, 3], value => value > 0)",
        "std.collection.every([1, 2, 3], value => value > 0)",
        "[1, 2, 3] | every(predicate: value => value > 0)",
        "[1, 2, 3] | std.collection.every(predicate: value => value > 0)",
        "every(rows: [1, 2, 3], predicate: value => value > 0)",
        "std.collection.every(predicate: value => value > 0, rows: [1, 2, 3])",
        "every([], value => value > 0)",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            true_value,
            "{expression}"
        );
    }
    for expression in [
        "exists([1, 2, 3], value => value == 2)",
        "std.collection.exists([1, 2, 3], value => value == 2)",
        "[1, 2, 3] | exists(predicate: value => value == 2)",
        "[1, 2, 3] | std.collection.exists(predicate: value => value == 2)",
        "exists(rows: [1, 2, 3], predicate: value => value == 2)",
        "std.collection.exists(predicate: value => value == 2, rows: [1, 2, 3])",
        "exists([], value => value == 2)",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            if expression.starts_with("exists([]") {
                false_value.clone()
            } else {
                true_value.clone()
            },
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            "exists([1, 2, 3], value => value > 3)",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        false_value
    );

    let source = "fn positive(value: Int) = value > 0; fn all(rows: [Int]) = every(rows, positive); fn any(rows: [Int]) = std.collection.exists(predicate: positive, rows: rows);";
    assert_eq!(
        call_module(source, "all([1, 2, 3])", Limits::default()).unwrap(),
        true_value
    );
    assert_eq!(
        call_module(source, "any([1, 2, 3])", Limits::default()).unwrap(),
        true_value
    );
}

#[test]
fn std_collection_every_and_exists_short_circuit_in_order_and_require_bool_callbacks() {
    assert_eq!(
        evaluate("every([1, 2, 3], value => if value <= 2 { value == 1 } else { 1 / 0 == 0 })"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("exists([1, 2, 3], value => if value <= 2 { value == 2 } else { 1 / 0 == 0 })"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "every([0, 1], value => if value == 0 { 1 / 0 == 0 } else { true })",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            "exists([1], value => 1)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            "every([1], () => true)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            "std.collection.exists(1, value => true)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            "every([1, 2, 3], value => true)",
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
fn root_every_and_exists_remain_shadowable_by_admitted_functions_and_locals() {
    assert_eq!(
        call_module(
            "fn every(value: Int) = value + 100; fn run() = every(1);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            "if true { let exists = value => value + 100; exists(1) } else { 0 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
}

#[test]
fn admitted_function_named_fail_shadows_the_intrinsic() {
    assert_eq!(
        call_module(
            "fn fail(value: Int) = value + 1; fn run() = fail(1);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::int(2.into())
    );
}

#[test]
fn root_one_remains_shadowable_and_does_not_change_relation_member_behavior() {
    assert_eq!(
        call_module(
            "fn one(value: Int) = value + 100; fn run() = one(1);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            "if true { let one = value => value + 100; one(1) } else { 0 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        code(evaluate_expression(
            "Note.one()",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
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
fn std_collection_sort_by_accepts_all_call_forms_and_preserves_stable_ties() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(1.into()),
        Raw::Int(2.into()),
        Raw::Int(3.into()),
    ]))
    .unwrap();
    for expression in [
        "sort_by([3, 1, 2], value => value)",
        "std.collection.sort_by([3, 1, 2], value => value)",
        "sort_by(rows: [3, 1, 2], key: value => value)",
        "std.collection.sort_by(rows: [3, 1, 2], key: value => value)",
        "std.collection.sort_by(key: value => value, rows: [3, 1, 2])",
        "[3, 1, 2] | sort_by(key: value => value)",
        "[3, 1, 2] | std.collection.sort_by(key: value => value)",
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            "fn key(value: Int) = value % 10; fn run() = std.collection.sort_by(rows: [12, 3, 1], key: key);",
            "run()",
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Int(12.into()),
            Raw::Int(3.into()),
        ]))
        .unwrap()
    );
    assert_eq!(
        evaluate("sort_by([3, 2, 1, 4], value => value % 2)"),
        Value::new(Raw::Array(vec![
            Raw::Int(2.into()),
            Raw::Int(4.into()),
            Raw::Int(3.into()),
            Raw::Int(1.into()),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_sort_by_orders_dates_and_preserves_equal_key_source_order() {
    let result = evaluate(
        "sort_by([{key: 2024-02-29, label: \"later\"}, {key: 2024-01-01, label: \"first\"}, {key: 2024-01-01, label: \"second\"}], row => row.key)",
    );
    assert_eq!(
        result,
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-01-01".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-01-01".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("second".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-02-29".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("later".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_sort_by_orders_normalized_instants_and_nanoseconds_stably() {
    let result = evaluate(
        "sort_by([{key: 1970-01-01T00:00:00.000000002Z, label: \"nano\"}, {key: 1970-01-01T05:30:00+05:30, label: \"offset-first\"}, {key: 1970-01-01T00:00:00Z, label: \"epoch-first\"}, {key: 1970-01-01T05:30:00+05:30, label: \"offset-second\"}], row => row.key)",
    );
    assert_eq!(
        result,
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("offset-first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("epoch-first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("offset-second".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(2.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("nano".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_sort_by_uses_float_total_order() {
    let rows = float_rows(&[
        0x7ff8_0000_0000_0000,
        0x3ff0_0000_0000_0000,
        0x0000_0000_0000_0000,
        0x8000_0000_0000_0000,
    ]);
    let environment = Environment::from([("rows".into(), rows)]);
    assert_eq!(
        evaluate_expression(
            "std.collection.sort_by(rows: rows, key: value => value)",
            &environment,
            Limits::default(),
        )
        .unwrap(),
        float_rows(&[
            0x8000_0000_0000_0000,
            0x0000_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
        ])
    );
}

#[test]
fn std_collection_sort_by_evaluates_callbacks_before_sorting_and_fails_closed() {
    assert_eq!(
        code(evaluate_expression(
            "sort_by([1, 0, 2], value => 10 / value)",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    for (expression, expected) in [
        ("sort_by([1], 1)", "ORNA-EVAL-TYPE"),
        ("sort_by([1], value => [value])", "ORNA-EVAL-TYPE"),
        ("sort_by([1])", "ORNA-EVAL-UNSUPPORTED"),
        (
            "sort_by([1, 2], value => if value == 1 { value } else { \"x\" })",
            "ORNA-EVAL-TYPE",
        ),
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
        code(evaluate_expression(
            "sort_by([1, 2, 3], value => value)",
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
            "sort_by([1], value => value)",
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let functions = functions_from_source(
        "fn key(value: Int) = Note.insert(value); fn run() = sort_by([3, 1], key);",
    );
    let mut effects = NoteEffects::default();
    assert_eq!(
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        effects.calls,
        vec![vec![Value::int(3.into())], vec![Value::int(1.into())]]
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
fn evaluates_canonical_date_literals_at_boundaries_and_leap_days() {
    for (source, expected) in [
        ("0001-01-01", "0001-01-01"),
        ("2000-02-29", "2000-02-29"),
        ("2024-02-29", "2024-02-29"),
        ("9999-12-31", "9999-12-31"),
    ] {
        assert_eq!(
            evaluate(source).raw(),
            &Raw::Tag(60001, Box::new(Raw::Text(expected.into()))),
            "{source}"
        );
    }
}

#[test]
fn date_literals_preserve_the_canonical_value_inside_collections_and_records() {
    assert_eq!(
        evaluate("[2024-02-29, {day: 9999-12-31}]").raw(),
        &Raw::Array(vec![
            Raw::Tag(60001, Box::new(Raw::Text("2024-02-29".into()))),
            Raw::Map(vec![(
                Raw::Text("day".into()),
                Raw::Tag(60001, Box::new(Raw::Text("9999-12-31".into()))),
            )]),
        ])
    );
    assert_eq!(
        evaluate("2024-02-29 == 2024-02-29"),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn malformed_date_is_rejected_lexically_before_evaluator_execution() {
    for source in ["0000-01-01", "2024-02-30", "2023-02-29", "2024-13-01"] {
        let errors = lex(source).expect_err("calendar-invalid input must fail lexing");
        assert!(
            errors.iter().any(|error| error.code == "ORNA-LEX-007"),
            "expected date diagnostic for {source}, got {errors:?}"
        );
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-PARSE"
        );
    }

    let malformed_ast = Expr::Literal {
        text: "0000-01-01".into(),
        kind: orna_syntax_v1::LiteralKind::Date,
        span: SyntaxSpan::new(0, 10),
    };
    assert_eq!(
        code(evaluate_parsed(
            &malformed_ast,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn date_token_class_is_disjoint_from_decimal_and_float_tokens() {
    let tokens = lex("2024-02-29 1.0 1.0f").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Date);
    assert_eq!(tokens[1].kind, TokenKind::Decimal);
    assert_eq!(tokens[2].kind, TokenKind::Float);
}

#[test]
fn instant_literals_normalize_offsets_and_emit_canonical_utc_components() {
    for (source, seconds, nanosecond) in [
        ("2024-02-29T00:00:00Z", 1_709_164_800, 0),
        ("2024-02-29T00:00:00.1+05:30", 1_709_145_000, 100_000_000),
        ("2024-02-28T18:30:00-05:30", 1_709_164_800, 0),
        ("1969-12-31T23:59:59.1Z", -1, 100_000_000),
    ] {
        assert_eq!(
            evaluate(source).raw(),
            &Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds.into()),
                    Raw::Int(nanosecond.into()),
                ])),
            ),
            "{source}"
        );
    }
}

#[test]
fn instant_literals_compare_by_normalized_utc_second_and_nanosecond() {
    assert_eq!(
        evaluate("2024-02-29T00:00:00Z == 2024-02-29T05:30:00+05:30"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1970-01-01T00:00:00.000000001Z < 1970-01-01T00:00:01Z"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1970-01-01T00:00:00.123456789Z > 1970-01-01T00:00:00.123456788Z"),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn instant_literals_preserve_precision_nesting_and_canonical_environment_values() {
    assert_eq!(
        evaluate("[1970-01-01T00:00:00Z, {instant: 1970-01-01T00:00:00.123456789Z}]").raw(),
        &Raw::Array(vec![
            Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
            ),
            Raw::Map(vec![(
                Raw::Text("instant".into()),
                Raw::Tag(
                    60002,
                    Box::new(Raw::Array(vec![
                        Raw::Int(0.into()),
                        Raw::Int(123_456_789.into()),
                    ])),
                ),
            )]),
        ])
    );

    let environment = Environment::from([(
        "instant".into(),
        Value::new(Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![Raw::Int((-1).into()), Raw::Int(1.into())])),
        ))
        .unwrap(),
    )]);
    assert_eq!(
        evaluate_expression("instant", &environment, Limits::default())
            .unwrap()
            .raw(),
        &Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![Raw::Int((-1).into()), Raw::Int(1.into())])),
        )
    );
}

#[test]
fn malformed_instant_literals_and_canonical_components_fail_closed() {
    for source in [
        "2024-02-29T24:00:00Z",
        "2024-02-29T12:60:00Z",
        "2024-02-29T12:00:60Z",
        "2024-02-29T12:00:00.1234567890Z",
        "2024-02-29T12:00:00+24:00",
    ] {
        let errors = lex(source).expect_err("malformed instant must fail lexing");
        assert!(
            errors.iter().any(|error| error.code == "ORNA-LEX-008"),
            "expected instant diagnostic for {source}, got {errors:?}"
        );
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-PARSE"
        );
    }

    let malformed_ast = Expr::Literal {
        text: "2024-02-29T12:00:60Z".into(),
        kind: orna_syntax_v1::LiteralKind::Instant,
        span: SyntaxSpan::new(0, 20),
    };
    assert_eq!(
        code(evaluate_parsed(
            &malformed_ast,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );

    for raw in [
        Raw::Tag(60002, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
        Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Int(1_000_000_000.into()),
            ])),
        ),
    ] {
        assert!(
            Value::new(raw).is_err(),
            "invalid Instant tag must fail closed"
        );
    }

    let environment = Environment::from([(
        "instant".into(),
        Value::new(Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![
                Raw::Int("9223372036854775808".parse().unwrap()),
                Raw::Int(0.into()),
            ])),
        ))
        .unwrap(),
    )]);
    assert_eq!(
        code(evaluate_expression(
            "instant",
            &environment,
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn canonical_duration_environment_values_round_trip_and_compare() {
    let duration = Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int((-1).into()),
            Raw::Int(500_000_000.into()),
        ])),
    );
    let later = Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int("9223372036854775808".parse().unwrap()),
            Raw::Int(0.into()),
        ])),
    );
    let environment = Environment::from([
        ("duration".into(), Value::new(duration.clone()).unwrap()),
        ("same".into(), Value::new(duration.clone()).unwrap()),
        ("later".into(), Value::new(later).unwrap()),
    ]);

    assert_eq!(
        evaluate_expression("duration", &environment, Limits::default())
            .unwrap()
            .raw(),
        &duration
    );
    assert_eq!(
        evaluate_expression("duration == same", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression("duration < later", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn malformed_canonical_duration_components_fail_closed() {
    for raw in [
        Raw::Tag(60005, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
        Raw::Tag(
            60005,
            Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int((-1).into())])),
        ),
        Raw::Tag(
            60005,
            Box::new(Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Int(1_000_000_000.into()),
            ])),
        ),
    ] {
        assert!(
            Value::new(raw).is_err(),
            "invalid Duration tag must fail closed"
        );
    }
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
fn finite_for_break_values_stop_iteration_and_preserve_loop_boundaries() {
    assert_eq!(
        evaluate(
            "if true { let total = 0; let result = for value in [1, 2, 3] { if value == 2 { break value * 10; }; total += value; }; [result, total] }"
        ),
        Value::new(Raw::Array(vec![Raw::Int(20.into()), Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        evaluate(
            "if true { let total = 0; for outer in [1, 2] { let inner = for value in [1, 2] { if value == 2 { break outer * 10; }; total += 1; }; total += inner; }; total }"
        ),
        Value::int(32.into())
    );
    assert_eq!(
        evaluate("if true { let result = for value in [1, 2, 3] { value; }; result }"),
        Value::unit()
    );
    assert_eq!(
        evaluate(
            "if true { let result = for value in [1, 2, 3] { if value == 2 { break; }; }; result }"
        ),
        Value::unit()
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { break 1; }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { while true { break 1; } }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
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
}

#[test]
fn fail_argument_propagates_a_return_transfer_before_error_matching() {
    assert_eq!(
        invoke(
            "fn run() = fail(if true { return 7; } else { 0 });",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(7.into())
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
    for source in ["1 in 1.0f..5.0f", "1 in 1..5.0f"] {
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

fn date_range(lower: Option<&str>, upper: Option<&str>, upper_inclusive: bool) -> Value {
    let endpoint = |value: Option<&str>| match value {
        Some(value) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Tag(60001, Box::new(Raw::Text(value.into()))),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn decimal_range(
    lower: Option<(i64, i64)>,
    upper: Option<(i64, i64)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(i64, i64)>| match value {
        Some((coefficient, exponent10)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Value::decimal(coefficient.into(), exponent10.into())
                    .unwrap()
                    .raw()
                    .clone(),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn float_range(lower: Option<f64>, upper: Option<f64>, upper_inclusive: bool) -> Value {
    let endpoint = |value: Option<f64>| match value {
        Some(value) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Float(value.to_bits()),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn instant_range(
    lower: Option<(i64, u32)>,
    upper: Option<(i64, u32)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(i64, u32)>| match value {
        Some((unix_seconds, nanosecond)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Tag(
                    60002,
                    Box::new(Raw::Array(vec![
                        Raw::Int(unix_seconds.into()),
                        Raw::Int(nanosecond.into()),
                    ])),
                ),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn duration(seconds: i64, nanosecond: u32) -> Value {
    duration_big(seconds.into(), nanosecond)
}

fn duration_big(seconds: BigInt, nanosecond: u32) -> Value {
    Value::new(Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int(seconds),
            Raw::Int(nanosecond.into()),
        ])),
    ))
    .unwrap()
}

fn duration_range(
    lower: Option<(i64, u32)>,
    upper: Option<(i64, u32)>,
    upper_inclusive: bool,
) -> Value {
    duration_range_big(
        lower.map(|(seconds, nanosecond)| (seconds.into(), nanosecond)),
        upper.map(|(seconds, nanosecond)| (seconds.into(), nanosecond)),
        upper_inclusive,
    )
}

fn duration_range_big(
    lower: Option<(BigInt, u32)>,
    upper: Option<(BigInt, u32)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(BigInt, u32)>| match value {
        Some((seconds, nanosecond)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                duration_big(seconds, nanosecond).raw().clone(),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

#[test]
fn decimal_ranges_are_canonical_membership_values_with_optional_bounds_and_ordering() {
    let half_open = decimal_range(Some((125, -2)), Some((25, -1)), false);
    assert_eq!(evaluate("1.25..2.5"), half_open);
    assert_eq!(
        evaluate("1.25 in 1.25..2.5"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("2.5 in 1.25..2.5"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("2.5 in 1.25..=2.5"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("-1.0 in ..1.25"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("3.0 in 2.5.."),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("(1.25..2.5) < (1.5..2.5)"),
        Value::new(Raw::Bool(true)).unwrap()
    );

    let environment =
        Environment::from([("range".into(), decimal_range(None, Some((125, -2)), false))]);
    assert_eq!(
        evaluate_expression("range", &environment, Limits::default()).unwrap(),
        decimal_range(None, Some((125, -2)), false)
    );
}

#[test]
fn decimal_ranges_allow_empty_values_but_reject_mixed_types_and_iteration() {
    assert_eq!(
        evaluate("1.25 in 1.25..1.25"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("1.25 in 2.5..1.25"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        "1.25..2",
        "1.25 in 1.25..2",
        "1 in 1.25..2.5",
        "2024-02-01..2.5",
        "2024-02-01T00:00:00Z..2.5",
        "(..1.25) < (1..)",
        "sort_by([(..1.25), (1..)], value => value)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let environment = Environment::from([(
        "range".into(),
        decimal_range(Some((125, -2)), Some((25, -1)), false),
    )]);
    assert_eq!(
        code(evaluate_expression(
            "if true { for value in range { value }; 0 }",
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn float_ranges_support_finite_membership_with_optional_bounds() {
    let half_open = float_range(Some(1.25), Some(2.5), false);
    assert_eq!(evaluate("1.25f..2.5f"), half_open);
    assert_eq!(
        evaluate("1.25f in 1.25f..2.5f"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("2.5f in 1.25f..2.5f"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("2.5f in 1.25f..=2.5f"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("-1.0f in ..1.25f"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("3.0f in 2.5f.."),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn float_ranges_reject_mixed_types_and_iteration_and_nan_is_not_a_member() {
    assert_eq!(
        evaluate("1.25f in 1.25f..1.25f"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("1.25f in 2.5f..1.25f"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in ["1.25f..2", "1.25f in 1.25f..2", "1 in 1.25f..2.5f"] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let range = float_range(Some(1.25), Some(2.5), false);
    let environment = Environment::from([
        ("range".into(), range),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression("nan in range", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            "if true { for value in range { value }; 0 }",
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn float_ranges_allow_non_finite_endpoints_and_total_ordering() {
    let whole = float_range(Some(f64::NEG_INFINITY), Some(f64::INFINITY), false);
    let nan_lower = float_range(Some(f64::from_bits(CANONICAL_NAN_BITS)), Some(2.5), false);
    let negative_zero = float_range(Some(-0.0), Some(1.0), false);
    let positive_zero = float_range(Some(0.0), Some(1.0), false);
    let finite_lower = float_range(Some(1.0), Some(2.5), false);
    let environment = Environment::from([
        ("whole".into(), whole.clone()),
        ("nan_lower".into(), nan_lower.clone()),
        ("negative_zero".into(), negative_zero),
        ("positive_zero".into(), positive_zero),
        ("finite_lower".into(), finite_lower),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression("1.0f in whole", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression("nan in whole", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression("1.0f in nan_lower", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression("nan in nan_lower", &environment, Limits::default()).unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression("nan in 1..2", &environment, Limits::default())
            .unwrap_err()
            .code(),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate_expression("finite_lower < nan_lower", &environment, Limits::default(),).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            "negative_zero < positive_zero",
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression("whole", &environment, Limits::default()).unwrap(),
        whole
    );
    assert_eq!(
        evaluate_expression("nan_lower", &environment, Limits::default()).unwrap(),
        nan_lower
    );
}

#[test]
fn date_ranges_are_canonical_membership_values_with_optional_bounds() {
    let half_open = date_range(Some("2024-02-01"), Some("2024-02-03"), false);
    assert_eq!(evaluate("2024-02-01..2024-02-03"), half_open);
    assert_eq!(
        evaluate("2024-02-01 in 2024-02-01..2024-02-03"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("2024-02-03 in 2024-02-01..2024-02-03"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("2024-02-03 in 2024-02-01..=2024-02-03"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("2024-01-01 in ..2024-02-01"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("2024-03-01 in 2024-02-01.."),
        Value::new(Raw::Bool(true)).unwrap()
    );

    let environment =
        Environment::from([("range".into(), date_range(None, Some("2024-02-01"), false))]);
    assert_eq!(
        evaluate_expression("range", &environment, Limits::default()).unwrap(),
        date_range(None, Some("2024-02-01"), false)
    );
}

#[test]
fn date_ranges_allow_empty_values_but_reject_mixed_bounds_and_iteration() {
    assert_eq!(
        evaluate("2024-02-01 in 2024-02-01..2024-02-01"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("2024-02-01 in 2024-02-03..2024-02-01"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        "2024-02-01..1",
        "2024-02-01 in 2024-02-01..1",
        "1 in 2024-02-01..2024-02-03",
    ] {
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
            "if true { for date in 2024-02-01..2024-02-03 { date }; 0 }",
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate("if true { let total = 0; for value in 1..=3 { total += value; }; total }"),
        Value::int(6.into())
    );
}

#[test]
fn instant_ranges_are_canonical_membership_values_with_optional_bounds_and_ordering() {
    let half_open = instant_range(Some((0, 0)), Some((1, 2)), false);
    assert_eq!(
        evaluate("1970-01-01T00:00:00Z..1970-01-01T00:00:01.000000002Z"),
        half_open
    );
    assert_eq!(
        evaluate("1970-01-01T01:00:00+01:00 in 1969-12-31T23:00:00Z..1970-01-01T00:00:00Z"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("1970-01-01T00:00:00Z in 1970-01-01T00:00:00Z..1970-01-01T00:00:01.000000002Z"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(
            "1970-01-01T00:00:01.000000002Z in 1970-01-01T00:00:00Z..1970-01-01T00:00:01.000000002Z"
        ),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(
            "1970-01-01T00:00:01.000000002Z in 1970-01-01T00:00:00Z..=1970-01-01T00:00:01.000000002Z"
        ),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1969-12-31T23:59:59Z in ..1970-01-01T00:00:00Z"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1970-01-01T00:00:02Z in 1970-01-01T00:00:01Z.."),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(
            "(1970-01-01T00:00:00Z..1970-01-01T00:00:01Z) < (1970-01-01T00:00:01Z..1970-01-01T00:00:02Z)"
        ),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn instant_ranges_allow_empty_values_but_reject_mixed_types_and_iteration() {
    assert_eq!(
        evaluate("1970-01-01T00:00:00Z in 1970-01-01T00:00:00Z..1970-01-01T00:00:00Z"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("1970-01-01T00:00:01Z in 1970-01-01T00:00:02Z..1970-01-01T00:00:01Z"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        "1970-01-01T00:00:00Z..1",
        "1970-01-01T00:00:00Z in 1970-01-01T00:00:00Z..1",
        "1 in 1970-01-01T00:00:00Z..1970-01-01T00:00:01Z",
        "1970-01-01T00:00:00Z..1.0f",
        "(..1970-01-01T00:00:00Z) < (1..)",
        "sort_by([(..1970-01-01T00:00:00Z), (1..)], value => value)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let environment = Environment::from([(
        "range".into(),
        instant_range(Some((0, 0)), Some((1, 0)), false),
    )]);
    assert_eq!(
        code(evaluate_expression(
            "if true { for instant in range { instant }; 0 }",
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn canonical_duration_ranges_support_membership_ordering_and_stable_collections() {
    let earlier = duration(-1, 500_000_000);
    let equal = duration(0, 0);
    let later = duration(0, 1);
    let huge_seconds = BigInt::from(i64::MAX) + BigInt::from(1);
    let huge = duration_big(huge_seconds.clone(), 0);
    let membership_range = duration_range_big(
        Some((BigInt::from(-1), 500_000_000)),
        Some((huge_seconds.clone(), 0)),
        true,
    );
    let lower_first = duration_range(Some((-1, 500_000_000)), Some((1, 0)), false);
    let lower_second = duration_range(Some((0, 0)), Some((1, 0)), false);
    let upper_first = duration_range(Some((0, 0)), Some((0, 1)), false);
    let upper_second = duration_range(Some((0, 0)), Some((1, 0)), false);
    let exclusive = duration_range(Some((0, 0)), Some((1, 0)), false);
    let inclusive = duration_range(Some((0, 0)), Some((1, 0)), true);
    let rows = Value::new(Raw::Array(vec![
        Raw::Map(vec![
            (Raw::Text("key".into()), later.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("later".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), equal.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("first".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), equal.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("second".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), earlier.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("earlier".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), huge.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("huge".into())),
        ]),
    ]))
    .unwrap();
    let environment = Environment::from([
        ("earlier".into(), earlier.clone()),
        ("equal".into(), equal.clone()),
        ("later".into(), later.clone()),
        ("huge".into(), huge.clone()),
        ("range".into(), membership_range),
        ("lower_first".into(), lower_first),
        ("lower_second".into(), lower_second),
        ("upper_first".into(), upper_first),
        ("upper_second".into(), upper_second),
        ("exclusive".into(), exclusive),
        ("inclusive".into(), inclusive),
        (
            "values".into(),
            Value::new(Raw::Array(vec![
                later.raw().clone(),
                equal.raw().clone(),
                earlier.raw().clone(),
                huge.raw().clone(),
            ]))
            .unwrap(),
        ),
        ("rows".into(), rows),
    ]);

    for (source, expected) in [
        ("earlier in range", Value::new(Raw::Bool(true)).unwrap()),
        ("equal in range", Value::new(Raw::Bool(true)).unwrap()),
        ("later in range", Value::new(Raw::Bool(true)).unwrap()),
        ("min(values)", Value::option(Some(earlier.clone())).unwrap()),
        ("max(values)", Value::option(Some(huge.clone())).unwrap()),
        ("huge in range", Value::new(Raw::Bool(true)).unwrap()),
        (
            "lower_first < lower_second",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "upper_first < upper_second",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "exclusive < inclusive",
            Value::new(Raw::Bool(true)).unwrap(),
        ),
    ] {
        assert_eq!(
            evaluate_expression(source, &environment, Limits::default()).unwrap(),
            expected,
            "{source}"
        );
    }
    assert_eq!(
        evaluate_expression(
            "sort_by(rows, row => row.key)",
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (Raw::Text("key".into()), earlier.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("earlier".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), equal.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("first".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), equal.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("second".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), later.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("later".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), huge.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("huge".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn canonical_duration_ranges_reject_mixed_endpoint_and_member_types() {
    let duration = duration(0, 0);
    let mixed_range = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), duration.raw().clone()])),
            ),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    let environment = Environment::from([
        ("duration".into(), duration),
        (
            "date_range".into(),
            date_range(Some("2024-01-01"), Some("2024-01-02"), false),
        ),
        ("mixed_range".into(), mixed_range),
    ]);
    for source in ["duration in date_range", "mixed_range"] {
        assert_eq!(
            code(evaluate_expression(source, &environment, Limits::default())),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
}

#[test]
fn range_ordering_validates_unbounded_endpoint_types_before_lexicographic_ordering() {
    assert_eq!(
        evaluate("(..2024-02-01) < (2024-02-01..)"),
        Value::new(Raw::Bool(true)).unwrap()
    );
    for source in [
        "(..2024-02-01) < (1..)",
        "sort_by([(..2024-02-01), (1..)], value => value)",
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
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
